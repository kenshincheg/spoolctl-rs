fn main() {
    let version = env!("CARGO_PKG_VERSION");
    let mut res = winres::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    res.set_manifest_file("assets/app.manifest");
    res.set("ProductName", "SpoolCtl");
    res.set("FileDescription", "Управление службой печати Windows");
    res.set("ProductVersion", version);
    res.set("FileVersion", version);
    if let Err(err) = res.compile() {
        // Icon/manifest embed needs a Windows RC toolchain (MSVC/llvm-rc).
        println!("cargo:warning=winres failed ({err}); EXE will build without resources");
    }
}
