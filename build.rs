//! embeds ryad.manifest, the app icon and version information.
//!
//! no windows sdk (rc.exe) and no build-dependency: the icon and version
//! resources are written as a .res file here, which the msvc linker takes
//! as an input like any object file.

use std::path::Path;

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo");
    let out = std::env::var("OUT_DIR").expect("set by cargo");
    let manifest = Path::new(&dir).join("ryad.manifest");
    let icon = Path::new(&dir).join("assets").join("ryad.ico");
    println!("cargo:rerun-if-changed=ryad.manifest");
    println!("cargo:rerun-if-changed=assets/ryad.ico");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());

    let ico = std::fs::read(&icon).expect("assets/ryad.ico");
    let mut res = Res::new();
    add_icon(&mut res, &ico);
    res.add(16, 1, 0x0030, &version_info()); // RT_VERSION
    let path = Path::new(&out).join("ryad.res");
    std::fs::write(&path, res.bytes).expect("write ryad.res");
    println!("cargo:rustc-link-arg-bins={}", path.display());
}

/// a .res file: a list of resources, each with a header (resinfo.h layout).
struct Res {
    bytes: Vec<u8>,
}

impl Res {
    fn new() -> Res {
        let mut r = Res { bytes: Vec::new() };
        r.add(0, 0, 0, &[]); // the empty leading entry every .res starts with
        r
    }

    /// `kind` and `name` are numeric ids (RT_* and resource id).
    fn add(&mut self, kind: u16, name: u16, flags: u16, data: &[u8]) {
        let b = &mut self.bytes;
        b.extend((data.len() as u32).to_le_bytes()); // DataSize
        b.extend(32u32.to_le_bytes()); // HeaderSize: fixed with numeric type and name
        b.extend([0xff, 0xff]);
        b.extend(kind.to_le_bytes());
        b.extend([0xff, 0xff]);
        b.extend(name.to_le_bytes());
        b.extend(0u32.to_le_bytes()); // DataVersion
        b.extend(flags.to_le_bytes()); // MemoryFlags
        b.extend(if kind == 0 { 0u16 } else { 0x0409u16 }.to_le_bytes()); // LanguageId: en-us
        b.extend(0u32.to_le_bytes()); // Version
        b.extend(0u32.to_le_bytes()); // Characteristics
        b.extend(data);
        while !b.len().is_multiple_of(4) {
            b.push(0);
        }
    }
}

/// an .ico file becomes one RT_ICON per image plus an RT_GROUP_ICON (id 1).
fn add_icon(res: &mut Res, ico: &[u8]) {
    let u16_at = |i: usize| u16::from_le_bytes([ico[i], ico[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([ico[i], ico[i + 1], ico[i + 2], ico[i + 3]]);
    let count = u16_at(4) as usize;
    let mut group = Vec::new();
    group.extend(0u16.to_le_bytes());
    group.extend(1u16.to_le_bytes()); // icon
    group.extend((count as u16).to_le_bytes());
    for n in 0..count {
        let e = 6 + n * 16;
        let (size, offset) = (u32_at(e + 8) as usize, u32_at(e + 12) as usize);
        res.add(3, (n + 1) as u16, 0x1010, &ico[offset..offset + size]); // RT_ICON
        group.extend(&ico[e..e + 8]); // width, height, colours, reserved, planes, bit count
        group.extend((size as u32).to_le_bytes());
        group.extend(((n + 1) as u16).to_le_bytes());
    }
    res.add(14, 1, 0x1030, &group); // RT_GROUP_ICON
}

/// VS_VERSIONINFO, so explorer's file properties name the product.
fn version_info() -> Vec<u8> {
    let v = |name: &str| std::env::var(name).ok().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    let (major, minor, patch) = (v("CARGO_PKG_VERSION_MAJOR"), v("CARGO_PKG_VERSION_MINOR"), v("CARGO_PKG_VERSION_PATCH"));
    let text = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let prerelease = !std::env::var("CARGO_PKG_VERSION_PRE").unwrap_or_default().is_empty();

    let mut fixed = Vec::new();
    for x in [
        0xFEEF04BDu32,                    // signature
        0x0001_0000,                      // struct version
        (major << 16) | minor,            // file version ms
        patch << 16,                      // file version ls
        (major << 16) | minor,            // product version ms
        patch << 16,                      // product version ls
        0x3F,                             // flags mask
        if prerelease { 0x2 } else { 0 }, // VS_FF_PRERELEASE
        0x0004_0004,                      // VOS_NT_WINDOWS32
        1,                                // VFT_APP
        0,
        0,
        0,
    ] {
        fixed.extend(x.to_le_bytes());
    }

    let strings: Vec<Vec<u8>> = [
        ("CompanyName", "lezneve"),
        ("FileDescription", "ryad"),
        ("FileVersion", text.as_str()),
        ("InternalName", "ryad"),
        ("OriginalFilename", "ryad.exe"),
        ("ProductName", "ryad"),
        ("ProductVersion", text.as_str()),
    ]
    .iter()
    .map(|(k, val)| {
        let w: Vec<u16> = val.encode_utf16().chain(Some(0)).collect();
        block(k, 1, w.len() as u16, &w.iter().flat_map(|c| c.to_le_bytes()).collect::<Vec<u8>>(), &[])
    })
    .collect();
    let table = block("040904b0", 1, 0, &[], &strings);
    let string_info = block("StringFileInfo", 1, 0, &[], &[table]);
    let translation = block("Translation", 0, 4, &0x04b0_0409u32.to_le_bytes(), &[]);
    let var_info = block("VarFileInfo", 1, 0, &[], &[translation]);
    block("VS_VERSION_INFO", 0, fixed.len() as u16, &fixed, &[string_info, var_info])
}

/// one version block: wLength, wValueLength, wType, key, padding, value,
/// padding, children. lengths include everything up to the last child.
fn block(key: &str, kind: u16, value_len: u16, value: &[u8], children: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![0, 0];
    b.extend(value_len.to_le_bytes());
    b.extend(kind.to_le_bytes());
    for c in key.encode_utf16().chain(Some(0)) {
        b.extend(c.to_le_bytes());
    }
    let pad = |b: &mut Vec<u8>| {
        while !b.len().is_multiple_of(4) {
            b.push(0);
        }
    };
    pad(&mut b);
    b.extend(value);
    for child in children {
        pad(&mut b);
        b.extend(child);
    }
    let len = b.len() as u16;
    b[0..2].copy_from_slice(&len.to_le_bytes());
    b
}
