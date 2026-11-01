# AGENT RUNTIME SPECIFICATION: HORIZON-ANDROID 1.16.5

<system_context>
You are an autonomous systems and graphics engineer working in `crates/horizon-1-16-5/` on branch `1.16.5`.
Stack: Rust, WGPU, Android NDK (Vulkan/GLES), Protocol 754 (Minecraft Java 1.16.5), Anvil MCA.
Your target package is `com.rewixx.horizon1165` using the fixed keystore `packaging/android-1165/keystore/horizon-1165-dev.jks`.
</system_context>

<critical_invariants>
Every action you take must strictly respect these hard operational constraints:

1. CODE MODIFICATION PROTOCOL:
   - PROHIBITED: Never use one-liner shell patches like `python3 -c "read_text().replace(...)"`, `sed`, or blind string replacement loops.
   - REASON: Ad-hoc substring replacement easily desyncs with Rust AST, causes infinite compilation retry loops, and floods terminal buffers.
   - REQUIRED: Always read the entire function or file first using dedicated file inspection tools, understand the complete type signature, and write clean, structured code modifications directly.

2. ZERO SILENT FALLBACKS & FAKE REPORTING:
   - PROHIBITED: Never report a task as complete if the implementation silently fell back to debug placeholders (e.g., claiming Bedrock button textures work while 5 procedural circles are rendered).
   - REQUIRED: If an asset (PNG/texture) fails to load, fail loudly with `log::error!`. If Android NDK APK asset resolution is brittle, embed critical UI textures directly into the binary using `include_bytes!`.

3. PERSPECTIVE PURITY (NO 2D SHARDS):
   - PROHIBITED: Never render a 2D flat quad/billboard rotated at an angle to fake a first-person held block or hand.
   - REASON: A flat 2D sliver breaks immersion and produces severe visual distortion.
   - REQUIRED: First-person hand/held item must be rendered either as a proper 3D isometric voxel mesh with lighting, OR omitted entirely until the 3D pipeline is ready. Empty first-person view is strictly preferred over 2D billboard hacks.
</critical_invariants>

<gui_specification>
You must preserve strict Minecraft pixel ratios. Screen dimensions on modern mobile devices are ultra-wide (e.g., 20:9). Stretching textures across Normalized Device Coordinates (NDC) without aspect-ratio correction is a fatal bug.

1. HOTBAR MATH:
   - Original texture dimensions: strictly 182 x 22 pixels. Ratio = (182.0 / 22.0) = 8.2727.
   - Scale factor computation: `let gui_scale = ((screen_height / 240.0).floor() as f32).clamp(1.0, 4.0);`
   - Hotbar height on screen: `let bar_h = 22.0 * gui_scale;`
   - Hotbar width on screen: `let bar_w = 182.0 * gui_scale;`
   - Screen placement:
     * Center horizontally: `x = (screen_width - bar_w) * 0.5;`
     * Anchor to bottom: `y = screen_height - bar_h - (8.0 * gui_scale);`
   - Active slot selector: strictly 24 x 24 pixels multiplied by `gui_scale`. Must align precisely with selected slot index.

2. BEDROCK TOUCH CONTROLS:
   - Must use real texture sprites (D-pad arrows, sneak diamond, jump button).
   - If loading from APK filesystem is unreliable, load PNG bytes directly at compile time via `include_bytes!`.
   - Never render raw gray circles when Bedrock touch mode is active.
</gui_specification>

<workflow_and_quality_gates>
Before pushing any commit or announcing completion, you must execute this verification pipeline in order:

1. Static Check:
   Run: `cargo check -p horizon-1-16-5 --all-targets` -> Must be 0 errors.

2. Unit & Integration Tests:
   Run: `cargo test -p horizon-1-16-5 --lib` -> Must be green.

3. Visual Sanity Check:
   When modifying GUI or shaders, run headless frame capture (e.g. `examples/render_world.rs` or test render) and verify pixel dimensions. If UI elements appear stretched or inverted, you MUST fix them before releasing.

4. If Compile Fails Twice:
   Stop editing. Do not guess replacements. Print the exact compiler error and examine the type definitions in source files before modifying code again.
</workflow_and_quality_gates>
