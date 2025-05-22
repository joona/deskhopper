// build.rs
fn main() {
    println!("cargo:rerun-if-changed=icon.ico");
    embed_resource::compile("icon.ico", embed_resource::NONE);
}
