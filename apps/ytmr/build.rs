//! Icône et description de `ytmr.exe` (Windows).
//!
//! En clair : l'Explorateur, la barre des tâches et « Applications
//! installées » montrent l'icône de ytmr et son nom au lieu de l'icône
//! générique d'un programme.

fn main() {
    println!("cargo:rerun-if-changed=../../packaging/windows/ytmr.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon("../../packaging/windows/ytmr.ico")
        .set("FileDescription", "ytmr")
        .set("ProductName", "ytmr")
        .set("CompanyName", "ytmr");
    if let Err(e) = resource.compile() {
        // Sans le compilateur de ressources (SDK Windows), l'exe reste valide,
        // simplement sans icône.
        println!("cargo:warning=icône de ytmr.exe non intégrée : {e}");
    }
}
