//! Block picking for the 1.16.5 target.

use crate::blocks::is_solid;
use crate::{BlockSource, Face};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockHit {
    pub position: [i32; 3],
    /// The face that was entered, i.e. the face the player clicked.
    pub face: Face,
}

/// Casts a ray through the block grid and returns the first solid block.
///
/// Amanatides–Woo voxel traversal: the ray advances one unit cube at a time,
/// always crossing the nearest axis-aligned plane first, so no block between
/// `origin` and `max_distance` is skipped even at glancing angles or when the
/// origin sits exactly on a cell boundary. Negative direction components are
/// handled by the floor/ceil arithmetic on the entry cell.
///
/// A ray starting inside a solid block returns that block immediately. There
/// is no entry face in that case, so `face` is the face opposite the dominant
/// direction component (e.g. `West` for a mostly-eastward ray), which is the
/// face such a ray would have crossed next.
pub fn raycast(
    world: &dyn BlockSource,
    origin: [f64; 3],
    direction: [f32; 3],
    max_distance: f64,
) -> Option<BlockHit> {
    if !max_distance.is_finite() || max_distance <= 0.0 {
        return None;
    }
    if origin.iter().any(|o| !o.is_finite()) || direction.iter().any(|d| !d.is_finite()) {
        return None;
    }
    if direction == [0.0, 0.0, 0.0] {
        return None;
    }

    let dir = [
        direction[0] as f64,
        direction[1] as f64,
        direction[2] as f64,
    ];

    // Cell containing the origin, and the direction of travel per axis.
    let mut cell = [
        origin[0].floor() as i32,
        origin[1].floor() as i32,
        origin[2].floor() as i32,
    ];
    let step = [
        if dir[0] > 0.0 {
            1
        } else if dir[0] < 0.0 {
            -1
        } else {
            0
        },
        if dir[1] > 0.0 {
            1
        } else if dir[1] < 0.0 {
            -1
        } else {
            0
        },
        if dir[2] > 0.0 {
            1
        } else if dir[2] < 0.0 {
            -1
        } else {
            0
        },
    ];

    // Parametric distance to advance one cell on each axis.
    let mut t_delta = [f64::INFINITY; 3];
    for axis in 0..3 {
        if dir[axis] != 0.0 {
            t_delta[axis] = (1.0 / dir[axis]).abs();
        }
    }

    // Distance to the first axis-aligned plane beyond the origin cell.
    let mut t_max = [f64::INFINITY; 3];
    for axis in 0..3 {
        if dir[axis] > 0.0 {
            t_max[axis] = (cell[axis] as f64 + 1.0 - origin[axis]) / dir[axis];
        } else if dir[axis] < 0.0 {
            t_max[axis] = (cell[axis] as f64 - origin[axis]) / dir[axis];
        }
    }

    // The ray starts inside a solid block: report it without stepping.
    if let Some(state) = world.state_at(cell[0], cell[1], cell[2]) {
        if is_solid(&state.name) {
            let dominant = dominant_axis(dir);
            return Some(BlockHit {
                position: cell,
                face: dominant.opposite(),
            });
        }
    }

    loop {
        // Cross the nearest plane first.
        let (axis, next_t) = if t_max[0] <= t_max[1] && t_max[0] <= t_max[2] {
            (0, t_max[0])
        } else if t_max[1] <= t_max[2] {
            (1, t_max[1])
        } else {
            (2, t_max[2])
        };
        if next_t > max_distance {
            return None;
        }

        // The face of the new cell that the ray entered through.
        let face = match axis {
            0 => {
                if step[0] > 0 {
                    Face::West
                } else {
                    Face::East
                }
            }
            1 => {
                if step[1] > 0 {
                    Face::Down
                } else {
                    Face::Up
                }
            }
            _ => {
                if step[2] > 0 {
                    Face::North
                } else {
                    Face::South
                }
            }
        };

        cell[axis] += step[axis];
        t_max[axis] += t_delta[axis];

        if let Some(state) = world.state_at(cell[0], cell[1], cell[2]) {
            if is_solid(&state.name) {
                return Some(BlockHit {
                    position: cell,
                    face,
                });
            }
        }
    }
}

/// The block coordinate a placement goes into for a hit.
pub fn placement(hit: &BlockHit) -> [i32; 3] {
    let offset = hit.face.offset();
    [
        hit.position[0] + offset[0],
        hit.position[1] + offset[1],
        hit.position[2] + offset[2],
    ]
}

/// The face whose normal best matches the direction, ties broken in the
/// `x`, then `y`, then `z` order.
fn dominant_axis(dir: [f64; 3]) -> Face {
    let ax = dir[0].abs();
    let ay = dir[1].abs();
    let az = dir[2].abs();
    if ax >= ay && ax >= az {
        if dir[0] >= 0.0 {
            Face::East
        } else {
            Face::West
        }
    } else if ay >= az {
        if dir[1] >= 0.0 { Face::Up } else { Face::Down }
    } else if dir[2] >= 0.0 {
        Face::South
    } else {
        Face::North
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlockState;

    /// A fixed grid: solid blocks at the given coordinates, air elsewhere.
    struct Grid {
        solid: Vec<[i32; 3]>,
    }

    impl BlockSource for Grid {
        fn state_at(&self, x: i32, y: i32, z: i32) -> Option<BlockState> {
            if self.solid.contains(&[x, y, z]) {
                Some(BlockState::new("minecraft:stone"))
            } else {
                Some(BlockState::new("minecraft:air"))
            }
        }
    }

    fn grid(solid: &[[i32; 3]]) -> Grid {
        Grid {
            solid: solid.to_vec(),
        }
    }

    #[test]
    fn hits_near_face_of_block() {
        let world = grid(&[[3, 0, 0]]);
        // Traveling east, the ray enters the block's west face.
        let hit = raycast(&world, [0.0, 0.5, 0.5], [1.0, 0.0, 0.0], 10.0).expect("ray should hit");
        assert_eq!(hit.position, [3, 0, 0]);
        assert_eq!(hit.face, Face::West);
    }

    #[test]
    fn misses_when_ray_passes_beside() {
        let world = grid(&[[3, 0, 0]]);
        // The block spans y 0..1, z 0..1; this ray passes above it.
        let hit = raycast(&world, [0.0, 1.5, 0.5], [1.0, 0.0, 0.0], 10.0);
        assert_eq!(hit, None);
    }

    #[test]
    fn max_distance_excludes_farther_block() {
        let world = grid(&[[2, 0, 0], [8, 0, 0]]);
        let hit =
            raycast(&world, [0.0, 0.5, 0.5], [1.0, 0.0, 0.0], 5.0).expect("near block is in range");
        assert_eq!(hit.position, [2, 0, 0]);
        // A shorter ray reaches neither.
        assert_eq!(raycast(&world, [0.0, 0.5, 0.5], [1.0, 0.0, 0.0], 1.5), None);
    }

    #[test]
    fn negative_direction_ray() {
        let world = grid(&[[-3, 0, 0]]);
        let hit = raycast(&world, [0.0, 0.5, 0.5], [-1.0, 0.0, 0.0], 10.0).expect("ray should hit");
        assert_eq!(hit.position, [-3, 0, 0]);
        // Moving west, the ray enters through the block's east face.
        assert_eq!(hit.face, Face::East);
    }

    #[test]
    fn placement_picks_neighbor() {
        let world = grid(&[[3, 0, 0]]);
        let hit = raycast(&world, [0.0, 0.5, 0.5], [1.0, 0.0, 0.0], 10.0).unwrap();
        assert_eq!(hit.face, Face::West);
        assert_eq!(placement(&hit), [2, 0, 0]);

        // Hitting the top of a block places above it.
        let hit = raycast(&world, [3.5, 3.0, 0.5], [0.0, -1.0, 0.0], 10.0).unwrap();
        assert_eq!(hit.position, [3, 0, 0]);
        assert_eq!(hit.face, Face::Up);
        assert_eq!(placement(&hit), [3, 1, 0]);
    }

    #[test]
    fn ray_inside_solid_returns_that_block() {
        let world = grid(&[[1, 0, 0]]);
        let hit = raycast(&world, [1.5, 0.5, 0.5], [1.0, 0.0, 0.0], 10.0)
            .expect("origin inside a solid block");
        assert_eq!(hit.position, [1, 0, 0]);
        // Dominant direction is east; the reported face is its opposite.
        assert_eq!(hit.face, Face::West);
    }

    #[test]
    fn boundary_origin_and_diagonal_do_not_skip() {
        // Origin exactly on a cell face, moving west: both sides are checked
        // and the solid cell immediately west of the boundary is found.
        let world = grid(&[[-1, 0, 0]]);
        let hit = raycast(&world, [0.0, 0.5, 0.5], [-1.0, 0.0, 0.0], 10.0)
            .expect("cell west of the boundary should be found");
        assert_eq!(hit.position, [-1, 0, 0]);
        assert_eq!(hit.face, Face::East);

        // A diagonal ray crossing every plane at a distinct time visits every
        // cell along its path, so the block three crossings in is found.
        let world = grid(&[[1, 2, 1]]);
        let hit = raycast(&world, [0.1, 0.1, 0.1], [1.0, 2.0, 1.0], 10.0)
            .expect("diagonal ray should hit");
        assert_eq!(hit.position, [1, 2, 1]);
        // Moving mostly up, the ray entered through the bottom face.
        assert_eq!(hit.face, Face::Down);
    }

    #[test]
    fn degenerate_inputs_return_none() {
        let world = grid(&[]);
        assert_eq!(
            raycast(&world, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0], 10.0),
            None
        );
        assert_eq!(raycast(&world, [0.0, 0.0, 0.0], [1.0, 0.0, 0.0], 0.0), None);
        assert_eq!(
            raycast(&world, [f64::NAN, 0.0, 0.0], [1.0, 0.0, 0.0], 10.0),
            None
        );
    }
}
