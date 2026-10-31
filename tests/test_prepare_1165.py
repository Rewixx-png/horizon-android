import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("prepare_1165", ROOT / "tools" / "prepare_1165.py")
prepare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepare)

LOCK = {
    "version": "1.16.5",
    "facts": {
        "protocol_version": 754,
        "world_version": 2586,
        "resource_pack_format": 6,
        "data_pack_format": 6,
        "java_major": 8,
    },
    "expected": {"blocks": 1, "block_states": 1, "items": 2, "registries": 2, "biomes": 1},
}


def reports(blocks=None, registries=None, commands=None, biomes=("plains",)):
    return {
        "blocks.json": blocks if blocks is not None else {
            "minecraft:air": {"states": [{"id": 0, "default": True}]},
        },
        "registries.json": registries if registries is not None else {
            "minecraft:block": {"default": "minecraft:air", "entries": {
                "minecraft:air": {"protocol_id": 0},
            }},
            "minecraft:item": {"default": "minecraft:air", "entries": {
                "minecraft:air": {"protocol_id": 0},
                "minecraft:stick": {"protocol_id": 1},
            }},
        },
        "commands.json": commands if commands is not None else {"type": "root", "children": {}},
        "biomes": sorted(biomes),
    }


def write_reports(root, value):
    folder = root / "reports"
    folder.mkdir()
    for name in ("blocks.json", "registries.json", "commands.json"):
        (folder / name).write_text(json.dumps(value[name]))
    (folder / "biomes").mkdir()
    for name in value["biomes"]:
        (folder / "biomes" / f"{name}.json").write_text("{}")
    return folder


def source():
    return {"version_json_sha1": "v", "client_sha1": "c", "server_sha1": "s",
            "client_mappings_sha1": "cm", "server_mappings_sha1": "sm", "asset_index_sha1": "a",
            "assets_downloaded": False}


class FactsTests(unittest.TestCase):
    def test_reads_facts_from_the_client_jar_not_the_version_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            jar = Path(directory) / "client.jar"
            with zipfile.ZipFile(jar, "w") as archive:
                archive.writestr("version.json", json.dumps({
                    "id": "1.16.5", "protocol_version": 754, "world_version": 2586,
                    "pack_version": 6, "stable": True,
                }))
            self.assertEqual(prepare.facts(LOCK, jar), {
                "version": "1.16.5", "protocol_version": 754, "world_version": 2586,
                "resource_pack_format": 6, "data_pack_format": 6, "stable": True, "java_major": 8,
            })

    def test_rejects_a_republished_version_with_different_facts(self):
        with tempfile.TemporaryDirectory() as directory:
            jar = Path(directory) / "client.jar"
            with zipfile.ZipFile(jar, "w") as archive:
                archive.writestr("version.json", json.dumps({
                    "id": "1.16.5", "protocol_version": 753, "world_version": 2586,
                    "pack_version": 6, "stable": True,
                }))
            with self.assertRaises(prepare.PrepareError):
                prepare.facts(LOCK, jar)


class TableTests(unittest.TestCase):
    def test_builds_and_verifies_the_m0_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            folder = write_reports(Path(directory), reports())
            tables = prepare.build_tables(LOCK, LOCK["facts"], folder, source())
            self.assertEqual(tables["blocks"]["entries"]["minecraft:air"]["default"], 0)
            self.assertEqual(tables["blocks"]["state_count"], 1)
            self.assertEqual(tables["items"]["count"], 2)
            self.assertEqual(tables["registries"]["count"], 2)
            self.assertEqual(tables["biomes"]["names"], ["plains"])
            prepare.verify_tables(LOCK, tables)

    def test_rejects_a_missing_default_and_duplicate_state_ids(self):
        blocks = {
            "minecraft:air": {"states": [{"id": 0, "default": True}]},
            "minecraft:stone": {"states": [{"id": 0}]},
        }
        with self.assertRaisesRegex(prepare.PrepareError, "appears twice"):
            prepare.check_blocks(blocks)
        blocks["minecraft:stone"]["states"][0]["id"] = 1
        with self.assertRaisesRegex(prepare.PrepareError, "no default"):
            prepare.check_blocks(blocks)
        blocks["minecraft:stone"]["states"][0]["default"] = True
        blocks["minecraft:air"]["states"].append({"id": 2, "default": True})
        with self.assertRaisesRegex(prepare.PrepareError, "more than one default"):
            prepare.check_blocks(blocks)

    def test_rejects_duplicate_protocol_ids_inside_one_registry(self):
        registries = {
            "minecraft:block": {"entries": {"minecraft:air": {"protocol_id": 0}}},
            "minecraft:item": {"entries": {
                "minecraft:air": {"protocol_id": 0}, "minecraft:stick": {"protocol_id": 0},
            }},
        }
        with self.assertRaisesRegex(prepare.PrepareError, "protocol id 0"):
            prepare.check_registries(registries)

    def test_rejects_block_report_registry_drift(self):
        blocks = {"minecraft:air": {"states": [{"id": 0, "default": True}]}}
        registries = reports()["registries.json"]
        registries["minecraft:block"]["entries"]["minecraft:stone"] = {"protocol_id": 1}
        with self.assertRaisesRegex(prepare.PrepareError, "block report and registry differ"):
            with tempfile.TemporaryDirectory() as directory:
                folder = write_reports(Path(directory), reports(blocks=blocks, registries=registries))
                prepare.build_tables(LOCK, LOCK["facts"], folder, source())

    def test_rejects_lock_count_drift(self):
        lock = json.loads(json.dumps(LOCK))
        lock["expected"]["blocks"] = 2
        with tempfile.TemporaryDirectory() as directory:
            folder = write_reports(Path(directory), reports())
            with self.assertRaisesRegex(prepare.PrepareError, "counts differ"):
                prepare.build_tables(lock, lock["facts"], folder, source())

    def test_verify_only_reports_a_missing_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(prepare.PrepareError, "No prepared bundle"):
                prepare.prepare(LOCK, Path(directory), None, False, True, True, 1)


class AssetTests(unittest.TestCase):
    def test_deduplicates_objects_and_builds_resource_urls(self):
        first, second = "a" * 40, "b" * 40
        index = {"objects": {
            "a": {"hash": first, "size": 2},
            "b": {"hash": first, "size": 2},
            "c": {"hash": second, "size": 3},
        }}
        objects = prepare.asset_objects(index)
        self.assertEqual([entry[1] for entry in objects], [f"{first[:2]}/{first}", f"{second[:2]}/{second}"])
        self.assertTrue(all(entry[0].startswith("https://resources.download.minecraft.net/") for entry in objects))

    def test_rejects_unsafe_asset_hashes(self):
        for value in ["../x", "A" * 40, "a" * 39]:
            with self.assertRaises(prepare.PrepareError):
                prepare.asset_objects({"objects": {"x": {"hash": value, "size": 1}}})


class LockTests(unittest.TestCase):
    def test_the_repository_lock_pins_the_1_16_5_contract(self):
        lock = json.loads((ROOT / "docs" / "versions" / "1.16.5.lock.json").read_text())
        self.assertEqual(lock["version"], "1.16.5")
        self.assertEqual(lock["facts"]["protocol_version"], 754)
        self.assertEqual(lock["facts"]["world_version"], 2586)
        self.assertEqual(lock["asset_index"]["id"], "1.16")
        self.assertEqual(lock["expected"], {
            "blocks": 763, "block_states": 17112, "items": 976, "registries": 46, "biomes": 79,
        })
        for key in ("version_json", "client", "server", "client_mappings", "server_mappings", "asset_index"):
            self.assertRegex(lock[key]["sha1"], r"^[0-9a-f]{40}$")
            self.assertGreater(lock[key]["size"], 0)


if __name__ == "__main__":
    unittest.main()
