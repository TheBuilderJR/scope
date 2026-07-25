fn main() {
    // Tauri embeds the web frontend (frontendDist) into the binary at compile
    // time. Without this, changing only files under ../src doesn't touch any
    // .rs file, so cargo skips recompilation and the binary keeps a stale copy
    // of the frontend. Watching the directory forces a rebuild + re-embed.
    println!("cargo:rerun-if-changed=../src");
    tauri_build::build()
}
