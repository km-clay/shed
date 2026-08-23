use std::path::Path;

fn main() {
  println!("cargo:rustc-check-cfg=cfg(linux_like)");
  println!("cargo:rerun-if-changed=include");
  let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
  if matches!(target_os.as_str(), "linux" | "android") {
    println!("cargo:rustc-cfg=linux_like");
  }

  let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
  let out_dir = std::env::var("OUT_DIR").unwrap();

  let mut generated = String::new();
  generated.push_str(&emit_include_group(&manifest_dir, "functions"));
  generated.push_str(&emit_include_group(&manifest_dir, "completions"));
  generated.push_str(&emit_include_group(&manifest_dir, "help"));

  let asset_dir = Path::new(&out_dir).join("embedded_assets.rs");
  std::fs::write(asset_dir, generated).unwrap();
}

fn emit_include_group(manifest_dir: &str, group: &str) -> String {
  let dir = Path::new(manifest_dir).join("include").join(group);
  let const_name = group.to_uppercase();

  let mut entries: Vec<(String, String)> = std::fs::read_dir(&dir)
    .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
    .filter_map(Result::ok)
    .filter(|e| e.path().is_file())
    .map(|e| {
      let fname = e.file_name();
      let stem = fname.to_string_lossy();
      let name = format!("{group}/{stem}");
      let path = e.path().to_string_lossy().to_string();
      (name, path)
    })
    .collect();

  entries.sort();

  use std::fmt::Write;
  let mut out = format!("pub static {const_name}: &[(&str, &[u8])] = &[\n");
  for (key, abs) in entries {
    writeln!(out, "  ({key:?}, include_bytes!({abs:?})),").unwrap();
  }
  out.push_str("];\n");
  out
}
