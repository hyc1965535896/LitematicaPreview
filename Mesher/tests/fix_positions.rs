use quartz_nbt::{io, NbtTag};

/// Rewrites every .litematic under SWAMP_DIR so any region with an absurd
/// absolute-world-coordinate offset (>|1024| on any axis) has its Position
/// zeroed and any negative-size corner form (Position = max corner) flipped
/// to the plain form (Position = min corner, all Size components positive).
/// This repairs litematica files saved from a placement with a large sub-region
/// offset; ordinary selection saves have Position=(0,0,0) and pass through
/// untouched. Intended to be run with `cargo test --release --test
/// fix_positions -- --ignored --nocapture` and `SWAMP_DIR` set.
#[test]
#[ignore = "set SWAMP_DIR and run with --ignored --nocapture"]
fn fix_region_positions_nbt() {
    let dir = std::env::var_os("SWAMP_DIR").expect("Set SWAMP_DIR");
    let mut fixed_count = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("litematic") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let (mut root, root_name) =
            io::read_nbt(&mut std::io::Cursor::new(&bytes), io::Flavor::GzCompressed)
                .or_else(|_| {
                    io::read_nbt(&mut std::io::Cursor::new(&bytes), io::Flavor::Uncompressed)
                })
                .unwrap();
        let Some(NbtTag::Compound(regions)) = root.inner_mut().get_mut("Regions") else {
            continue;
        };
        let mut file_fixed = false;
        for (_, region_tag) in regions.inner_mut() {
            let NbtTag::Compound(region) = region_tag else { continue };
            let Some(NbtTag::Compound(position)) = region.inner_mut().get_mut("Position") else {
                continue;
            };
            let mut get = |key: &str| match position.inner_mut().get(key) {
                Some(NbtTag::Int(value)) => *value,
                _ => 0,
            };
            let pos = (get("x"), get("y"), get("z"));
            let abs = pos.0.abs().max(pos.1.abs()).max(pos.2.abs());
            if abs <= 1024 {
                continue;
            }
            println!(
                "fixing {}: position {:?} -> (0, 0, 0)",
                path.file_name().unwrap().to_string_lossy(),
                pos
            );
            position.insert("x", NbtTag::Int(0));
            position.insert("y", NbtTag::Int(0));
            position.insert("z", NbtTag::Int(0));
            if let Some(NbtTag::Compound(size)) = region.inner_mut().get_mut("Size") {
                for axis in ["x", "y", "z"] {
                    if let Some(NbtTag::Int(value)) = size.inner_mut().get_mut(axis) {
                        *value = value.abs();
                    }
                }
            }
            file_fixed = true;
        }
        if !file_fixed {
            continue;
        }
        let backup = path.with_extension("litematic.bak");
        std::fs::copy(&path, &backup).unwrap();
        let mut output = Vec::new();
        io::write_nbt(&mut output, Some(&root_name), &root, io::Flavor::GzCompressed).unwrap();
        std::fs::write(&path, &output).unwrap();
        println!(
            "  written {} ({} bytes, was {}); backup at {}",
            path.display(),
            output.len(),
            bytes.len(),
            backup.file_name().unwrap().to_string_lossy()
        );
        fixed_count += 1;
    }
    println!("{fixed_count} file(s) fixed");
}
