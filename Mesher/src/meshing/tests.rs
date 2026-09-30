use super::*;
use crate::parts;

mod fixtures;
use fixtures::{schematic, test_pack};

fn area(mesh: &MeshOutput) -> f64 {
    parts(mesh)
        .flat_map(|(layer, _, _)| {
            layer.indices.chunks_exact(3).map(move |indices| {
                let a = layer.positions[indices[0] as usize];
                let b = layer.positions[indices[1] as usize];
                let c = layer.positions[indices[2] as usize];
                let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
                let cross = [
                    u[1] * v[2] - u[2] * v[1],
                    u[2] * v[0] - u[0] * v[2],
                    u[0] * v[1] - u[1] * v[0],
                ];
                (cross.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>()).sqrt() / 2.0
            })
        })
        .sum()
}

type VertexSignature = [i32; 10];
type TriangleSignature = (u32, [VertexSignature; 3]);

fn triangles(mesh: &MeshOutput) -> Vec<TriangleSignature> {
    let mut result = Vec::new();
    for (layer, _, alpha) in parts(mesh) {
        for indices in layer.indices.chunks_exact(3) {
            let mut vertices = [[0; 10]; 3];
            for (slot, &index) in vertices.iter_mut().zip(indices) {
                let index = index as usize;
                let values = layer.positions[index]
                    .into_iter()
                    .chain(layer.normals[index])
                    .chain(layer.colors[index]);
                for (out, value) in slot.iter_mut().zip(values) {
                    *out = (value * 1_000_000.0).round() as i32;
                }
            }
            vertices.sort_unstable();
            result.push((alpha, vertices));
        }
    }
    result.sort_unstable();
    result
}

#[test]
fn adjacent_core_blocks_have_no_hidden_boundary_faces_with_greedy() {
    let pack = test_pack();
    let schematic = schematic(&[(-1, 0, 0, "minecraft:stone"), (0, 0, 0, "minecraft:stone")]);
    let config = MeshConfig::new().with_greedy_meshing(true);
    let chunks = ChunkMeshes::new(schematic, &pack, &config, Some(1), || Ok(())).unwrap();
    let mut visible_area = 0.0;
    for chunk in chunks {
        let mesh = chunk.unwrap();
        visible_area += area(&mesh);
        assert!(mesh
            .greedy_materials
            .iter()
            .any(|m| !m.opaque.indices.is_empty()));
        for (layer, _, _) in parts(&mesh) {
            for indices in layer.indices.chunks_exact(3) {
                assert!(
                    !indices
                        .iter()
                        .all(|&i| layer.positions[i as usize][0] == -0.5),
                    "the shared internal plane must not be emitted"
                );
            }
        }
    }
    assert!((visible_area - 10.0).abs() < 1e-6);
}

#[test]
fn corner_halo_preserves_boundary_ambient_occlusion() {
    let pack = test_pack();
    let schematic = schematic(&[
        (-1, 0, -1, "minecraft:stone"),
        (0, 1, -1, "minecraft:stone"),
        (-1, 1, 0, "minecraft:stone"),
        (0, 1, 0, "minecraft:stone"),
    ]);
    let config = MeshConfig::new().with_greedy_meshing(false);
    let whole = schematic.to_mesh(&pack, &config).unwrap();
    let unshaded = schematic
        .to_mesh(&pack, &config.clone().with_ambient_occlusion(false))
        .unwrap();
    assert_ne!(
        triangles(&whole),
        triangles(&unshaded),
        "fixture must exercise AO"
    );
    let mut actual = Vec::new();
    for mesh in ChunkMeshes::new(schematic, &pack, &config, Some(1), || Ok(())).unwrap() {
        actual.extend(triangles(&mesh.unwrap()));
    }
    actual.sort_unstable();
    assert_eq!(actual, triangles(&whole));
}

#[test]
fn liquid_diagonal_above_and_transparency_match_whole_geometry() {
    let pack = test_pack();
    let mut schematic = schematic(&[
        (-1, 0, -1, "minecraft:water"),
        (0, 0, -1, "minecraft:water"),
        (0, 1, 0, "minecraft:water"),
        (-1, 0, 0, "minecraft:stone"),
        (-1, 3, 0, "minecraft:glass"),
        (0, 3, 0, "minecraft:glass"),
        (-1, 5, 0, "minecraft:oak_leaves"),
        (0, 5, 0, "minecraft:oak_leaves"),
    ]);
    schematic.set_block(
        0,
        0,
        0,
        &BlockState::new("minecraft:water").with_property("level", "5"),
    );
    let config = MeshConfig::new().with_greedy_meshing(false);
    let whole = schematic.to_mesh(&pack, &config).unwrap();
    assert!(parts(&whole).any(|(_, _, alpha)| alpha == 1));
    assert!(parts(&whole).any(|(_, _, alpha)| alpha == 2));
    let mut actual = Vec::new();
    for mesh in ChunkMeshes::new(schematic, &pack, &config, Some(1), || Ok(())).unwrap() {
        actual.extend(triangles(&mesh.unwrap()));
    }
    actual.sort_unstable();
    assert_eq!(actual, triangles(&whole));
}

#[test]
fn dynamic_particles_entities_and_position_keys_keep_one_atlas_layout() {
    let pack = test_pack();
    let mut schematic = schematic(&[
        (-65, 0, 0, "minecraft:torch"),
        (0, 0, 0, "minecraft:end_rod"),
        (64, 0, 0, "minecraft:player_head"),
        (128, 0, 0, "minecraft:player_head"),
        (192, 0, 0, "minecraft:unknown_texture"),
    ]);
    schematic.add_entity(nucleation::Entity::new(
        "minecraft:armor_stand".into(),
        (256.0, 0.0, 0.0),
    ));
    let config = MeshConfig::new().with_greedy_meshing(false);
    let chunks = ChunkMeshes::new(schematic, &pack, &config, Some(64), || Ok(())).unwrap();
    let atlas = chunks.atlas.clone();
    for path in [
        "_particle/flame",
        "_particle/glitter",
        "_player_head/64_0_0",
        "_player_head/128_0_0",
        "entity/armorstand/wood",
        "__missing__",
    ] {
        assert!(atlas.contains(path), "global atlas must contain {path}");
    }
    for mesh in chunks {
        let mesh = mesh.unwrap();
        assert_eq!(
            (mesh.atlas.width, mesh.atlas.height),
            (atlas.width, atlas.height)
        );
        assert_eq!(mesh.atlas.pixels, atlas.pixels);
        assert_eq!(mesh.atlas.regions.len(), atlas.regions.len());
        for (path, region) in &atlas.regions {
            let actual = mesh.atlas.get_region(path).unwrap();
            assert_eq!(
                [actual.u_min, actual.v_min, actual.u_max, actual.v_max],
                [region.u_min, region.v_min, region.u_max, region.v_max]
            );
        }
        if mesh.chunk_coord == Some((3, 0, 0)) {
            let region = atlas.get_region("__missing__").unwrap();
            for (layer, _, _) in parts(&mesh) {
                assert!(layer.uvs.iter().all(|uv| uv[0] >= region.u_min
                    && uv[0] <= region.u_max
                    && uv[1] >= region.v_min
                    && uv[1] <= region.v_max));
            }
        }
    }
}

#[test]
fn greedy_mode_keeps_cutouts_and_repeat_uvs() {
    let pack = test_pack();
    let schematic = schematic(&[
        (0, 0, 0, "minecraft:stone"),
        (1, 0, 0, "minecraft:stone"),
        (0, 3, 0, "minecraft:oak_leaves"),
        (1, 3, 0, "minecraft:oak_leaves"),
        (0, 5, 0, "minecraft:glass"),
        (1, 5, 0, "minecraft:glass"),
    ]);
    let config = MeshConfig::new().with_greedy_meshing(true);
    let mesh = ChunkMeshes::new(schematic, &pack, &config, Some(64), || Ok(()))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    assert!(parts(&mesh).any(|(_, _, alpha)| alpha == 1));
    assert!(parts(&mesh).any(|(_, _, alpha)| alpha == 2));
    assert!(mesh
        .greedy_materials
        .iter()
        .flat_map(|material| material.opaque.uvs.iter())
        .any(|uv| uv[0] > 1.0 || uv[1] > 1.0));
    assert!((area(&mesh) - 30.0).abs() < 1e-6);
}

#[test]
fn overlapping_regions_and_colocated_entities_keep_all_geometry() {
    let pack = test_pack();
    let mut schematic = schematic(&[(-1, 0, 0, "minecraft:stone")]);
    let mut overlap = nucleation::Region::new("overlap".into(), (-1, 0, 0), (-2, 1, 1));
    overlap.set_block(-1, 0, 0, &BlockState::new("minecraft:glass"));
    overlap.set_block(-2, 0, 0, &BlockState::new("minecraft:oak_leaves"));
    overlap.entities.push(Entity::new(
        "minecraft:armor_stand".into(),
        (-0.5, 0.0, 0.5),
    ));
    schematic.other_regions.insert("overlap".into(), overlap);
    let config = MeshConfig::new().with_greedy_meshing(false);
    let expected = schematic.to_mesh(&pack, &config).unwrap();
    let mut actual = Vec::new();
    for mesh in ChunkMeshes::new(schematic, &pack, &config, Some(1), || Ok(())).unwrap() {
        actual.extend(triangles(&mesh.unwrap()));
    }
    actual.sort_unstable();
    assert_eq!(actual, triangles(&expected));
}

#[test]
fn halo_handles_negative_division_and_extreme_coordinate_boundaries() {
    let mut schematic = UniversalSchematic::new("coordinate edges".into());
    let positions = [
        (i32::MIN, i32::MIN, i32::MIN),
        (i32::MIN + 1, i32::MIN + 1, i32::MIN + 1),
        (-65, -65, -65),
        (-64, -64, -64),
        (-63, -63, -63),
        (i32::MAX - 1, i32::MAX - 1, i32::MAX - 1),
        (i32::MAX, i32::MAX, i32::MAX),
    ];
    for (index, &(x, y, z)) in positions.iter().enumerate() {
        let name = format!("region {index}");
        let mut region = nucleation::Region::new(name.clone(), (x, y, z), (1, 1, 1));
        region.set_block(x, y, z, &BlockState::new("minecraft:stone"));
        schematic.other_regions.insert(name, region);
    }
    for size in [1, 64] {
        let source =
            CompactBlocks::from_schematic(schematic.clone(), Some(size), None, false, &|| Ok(()))
                .unwrap();
        for &(coord, _) in &source.chunks {
            let (min, max) = chunk_bounds(coord, size);
            let mut expected: Vec<_> = positions
                .iter()
                .copied()
                .filter(|&(x, y, z)| {
                    [x, y, z].into_iter().enumerate().all(|(axis, value)| {
                        i64::from(value) >= min[axis] - 1 && i64::from(value) < max[axis] + 1
                    })
                })
                .collect();
            let mut actual: Vec<_> = source
                .context(coord, Some(size))
                .iter()
                .map(|(pos, _)| (pos.x, pos.y, pos.z))
                .collect();
            expected.sort_unstable();
            actual.sort_unstable();
            assert_eq!(actual, expected, "halo for {coord:?}, size {size}");
        }
    }
}

#[test]
fn armor_stand_pose_and_equipment_survive_native_meshing() {
    let pack = test_pack();
    let mut plain = schematic(&[(-1, 0, 0, "minecraft:stone")]);
    plain.add_entity(Entity::new("minecraft:armor_stand".into(), (0.0, 0.0, 0.0)));
    let mut posed = plain.clone();
    let mut entity = Entity::armor_stand(
        (0.0, 0.0, 0.0),
        90.0,
        nucleation::ArmorStandEquipment::full_set("diamond"),
    );
    entity.nbt.insert(
        "RightArmPose".into(),
        NbtValue::List(vec![
            NbtValue::Float(45.0),
            NbtValue::Float(0.0),
            NbtValue::Float(90.0),
        ]),
    );
    posed.default_region.entities = vec![entity];
    let config = MeshConfig::new().with_greedy_meshing(false);
    let expected = posed.to_mesh(&pack, &config).unwrap();
    assert_ne!(
        triangles(&expected),
        triangles(&plain.to_mesh(&pack, &config).unwrap())
    );
    let mut actual = Vec::new();
    for mesh in ChunkMeshes::new(posed, &pack, &config, Some(1), || Ok(())).unwrap() {
        actual.extend(triangles(&mesh.unwrap()));
    }
    actual.sort_unstable();
    assert_eq!(actual, triangles(&expected));
}

#[test]
fn default_block_properties_select_geometry_and_explicit_properties_override() {
    use schematic_mesher::resource_pack::{BlockstateDefinition, ModelVariant};

    let mut pack = test_pack();
    let variant = |model: &str| {
        vec![ModelVariant {
            model: model.into(),
            x: 0,
            y: 0,
            uvlock: false,
            weight: 1,
        }]
    };
    pack.pack_mut().add_blockstate(
        "minecraft",
        "red_mushroom_block",
        BlockstateDefinition::Variants(HashMap::from([
            ("north=true".into(), variant("block/stone")),
            ("north=false".into(), variant("block/torch")),
        ])),
    );
    let bare = schematic(&[(0, 0, 0, "minecraft:red_mushroom_block")]);
    let mut explicit = bare.clone();
    explicit.set_block(
        0,
        0,
        0,
        &BlockState::new("minecraft:red_mushroom_block").with_property("north", "false"),
    );
    let config = MeshConfig::new().with_greedy_meshing(false);
    let visible_area = |schematic| {
        ChunkMeshes::new(schematic, &pack, &config, Some(1), || Ok(()))
            .unwrap()
            .map(|mesh| area(&mesh.unwrap()))
            .sum::<f64>()
    };
    assert!((visible_area(bare) - 6.0).abs() < 1e-6);
    assert_eq!(visible_area(explicit), 0.0);
}

#[test]
fn greedy_mode_preserves_mixed_cutout_faces_and_missing_texture_pixels() {
    use schematic_mesher::types::Direction;

    let mut pack = test_pack();
    let model = pack
        .pack_mut()
        .models
        .get_mut("minecraft")
        .unwrap()
        .get_mut("block/stone")
        .unwrap();
    model.elements[0]
        .faces
        .get_mut(&Direction::Up)
        .unwrap()
        .texture = "block/oak_leaves".into();
    let schematic = schematic(&[
        (0, 0, 0, "minecraft:stone"),
        (1, 0, 0, "minecraft:stone"),
        (0, 3, 0, "minecraft:unknown_texture"),
    ]);
    let build = |greedy| {
        ChunkMeshes::new(
            schematic.clone(),
            &pack,
            &MeshConfig::new().with_greedy_meshing(greedy),
            Some(64),
            || Ok(()),
        )
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
    };
    let greedy = build(true);
    let unmerged = build(false);
    assert_eq!(triangles(&greedy), triangles(&unmerged));
    assert!(greedy
        .cutout
        .normals
        .iter()
        .all(|normal| *normal == [0.0, 1.0, 0.0]));
    assert_eq!(greedy.cutout.indices.len() / 3, 4);
    let missing = greedy.atlas.get_region("__missing__").unwrap();
    let mut missing_vertices = 0;
    for (position, uv) in greedy.opaque.positions.iter().zip(&greedy.opaque.uvs) {
        if position[1] > 2.0 {
            missing_vertices += 1;
            assert!(uv[0] >= missing.u_min && uv[0] <= missing.u_max);
            assert!(uv[1] >= missing.v_min && uv[1] <= missing.v_max);
        }
    }
    assert_eq!(missing_vertices, 24);
}

#[test]
fn generated_texture_cannot_repack_an_already_shared_atlas() {
    let pack = test_pack();
    let block = InputBlock::new("minecraft:torch");
    let config = mesher_config(&MeshConfig::new().with_greedy_meshing(false));
    let atlas = atlas::build(pack.pack(), &config, std::iter::empty()).unwrap();
    let position = BlockPosition::new(0, 0, 0);
    let palette = [block];
    let result = builder::mesh(
        pack.pack(),
        &config,
        &atlas,
        &palette,
        &[false],
        &[(position, 0)],
        &[(position, &palette[0])],
        BoundingBox::new([0.0; 3], [1.0; 3]),
    );
    assert!(
        result.is_err(),
        "a late generated texture must never change earlier chunks' UV layout"
    );
}

#[test]
fn original_builder_keeps_static_and_dynamic_animation_metadata() {
    use schematic_mesher::resource_pack::AnimationMeta;

    let mut pack = test_pack();
    pack.pack_mut()
        .textures
        .get_mut("minecraft")
        .unwrap()
        .get_mut("block/stone")
        .unwrap()
        .apply_mcmeta(AnimationMeta {
            frametime: 3,
            interpolate: true,
            frames: None,
            frame_width: Some(16),
            frame_height: Some(8),
        });
    let mesh = ChunkMeshes::new(
        schematic(&[(0, 0, 0, "minecraft:stone"), (2, 0, 0, "minecraft:torch")]),
        &pack,
        &MeshConfig::new().with_greedy_meshing(false),
        Some(64),
        || Ok(()),
    )
    .unwrap()
    .next()
    .unwrap()
    .unwrap();
    for path in ["block/stone", "_particle/flame"] {
        let region = mesh.atlas.get_region(path).unwrap();
        let atlas_x = (region.u_min * mesh.atlas.width as f32).round() as u32;
        let atlas_y = (region.v_min * mesh.atlas.height as f32).round() as u32;
        let animation = mesh
            .animated_textures
            .iter()
            .find(|animation| animation.atlas_x == atlas_x && animation.atlas_y == atlas_y)
            .unwrap();
        assert!(animation.frame_count > 1);
        let image = image::load_from_memory(&animation.sprite_sheet_png).unwrap();
        assert_eq!(image.width(), animation.frame_width);
        assert_eq!(
            image.height(),
            animation.frame_height * animation.frame_count
        );
        if path == "block/stone" {
            assert_eq!(animation.frametime, 3);
            assert!(animation.interpolate);
        }
    }
}

#[test]
fn unseparated_mesh_preserves_full_context_materials_and_shared_atlas() {
    let pack = test_pack();
    let schematic = schematic(&[
        (63, 0, 0, "minecraft:water"),
        (64, 1, 1, "minecraft:water"),
        (64, 0, 0, "minecraft:stone"),
        (63, 2, 1, "minecraft:oak_leaves"),
        (64, 3, 0, "minecraft:glass"),
        (63, 3, 0, "minecraft:glass"),
    ]);
    let config = MeshConfig::new().with_greedy_meshing(false);
    let expected = schematic.to_mesh(&pack, &config).unwrap();
    let mut chunks = ChunkMeshes::new(schematic, &pack, &config, None, || Ok(())).unwrap();
    let atlas = chunks.atlas.clone();
    let actual = chunks.next().unwrap().unwrap();
    assert!(chunks.next().is_none());
    assert_eq!(actual.chunk_coord, None);
    assert_eq!(triangles(&actual), triangles(&expected));
    assert_eq!(actual.atlas.pixels, atlas.pixels);
    assert!(parts(&actual).any(|(_, _, alpha)| alpha == 1));
    assert!(parts(&actual).any(|(_, _, alpha)| alpha == 2));
}

#[test]
fn unseparated_dense_culler_rejects_unrepresentable_sparse_bounds_before_allocation() {
    let pack = test_pack();
    let config = MeshConfig::new();
    for positions in [
        vec![(i32::MIN, 0, 0)],
        vec![(0, 0, 0), (i32::MAX - 1, 0, 0)],
        vec![
            (-2_000_000, -2_000_000, -2_000_000),
            (2_000_000, 2_000_000, 2_000_000),
        ],
    ] {
        let mut schematic = UniversalSchematic::new("sparse".into());
        for (index, position) in positions.into_iter().enumerate() {
            let name = format!("region {index}");
            let mut region = nucleation::Region::new(name.clone(), position, (1, 1, 1));
            region.set_block(
                position.0,
                position.1,
                position.2,
                &BlockState::new("minecraft:stone"),
            );
            schematic.other_regions.insert(name, region);
        }
        let error = ChunkMeshes::new(schematic, &pack, &config, None, || Ok(()))
            .unwrap()
            .next()
            .unwrap()
            .err()
            .unwrap();
        assert!(error.starts_with("剔除"), "{error}");
    }
}

#[test]
fn parallel_dense_conversion_preserves_coordinates_and_palette_order_across_windows() {
    let schematic = schematic(&[
        (-20_000, -1, -2, "minecraft:stone"),
        (0, -1, -2, "minecraft:glass"),
        (20_000, -1, -2, "minecraft:torch"),
    ]);
    let serial =
        CompactBlocks::from_schematic(schematic.clone(), Some(16), None, false, &|| Ok(()))
            .unwrap();
    let signature = |source: &CompactBlocks| {
        source
            .chunks
            .iter()
            .map(|(coord, blocks)| {
                (
                    *coord,
                    blocks
                        .iter()
                        .map(|(pos, state)| {
                            (
                                pos.x,
                                pos.y,
                                pos.z,
                                source.palette[*state as usize].name.clone(),
                            )
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };
    for speed_first in [false, true] {
        let parallel = CompactBlocks::from_schematic(
            schematic.clone(),
            Some(16),
            Some(2),
            speed_first,
            &|| Ok(()),
        )
        .unwrap();
        assert_eq!(serial.block_count(), parallel.block_count());
        assert_eq!(signature(&serial), signature(&parallel));
    }
}

#[test]
fn speed_first_admits_requested_mesh_workers_despite_large_contexts() {
    let pack = test_pack();
    let config = MeshConfig::new().with_greedy_meshing(true);
    let mut model = UniversalSchematic::new("dense chunks".into());
    let stone = BlockState::new("minecraft:stone");
    for x in 0..64 {
        for y in 0..16 {
            for z in 0..16 {
                model.set_block(x, y, z, &stone);
            }
        }
    }
    let mut meshes = ChunkMeshes::new(model, &pack, &config, Some(16), || Ok(())).unwrap();
    // Each full chunk exceeds the memory-first context admission budget.
    assert_eq!(meshes.worker_admission(4, false), 1);
    assert_eq!(meshes.worker_admission(4, true), 4);
    let expected: Vec<_> = (0..meshes.source.chunk_count())
        .map(|index| {
            let mesh = meshes.mesh_at(index).unwrap();
            (mesh.chunk_coord, triangles(&mesh))
        })
        .collect();
    let mut actual = Vec::new();
    meshes
        .consume(
            Some(4),
            true,
            |mesh| {
                actual.push((mesh.chunk_coord, triangles(&mesh)));
                Ok(())
            },
            &|| Ok(()),
        )
        .unwrap();
    assert_eq!(actual, expected);
}
