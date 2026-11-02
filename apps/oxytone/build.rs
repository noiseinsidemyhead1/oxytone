//! Icône et description de `oxytone.exe` (Windows).
//!
//! En clair : l'Explorateur, la barre des tâches et « Applications
//! installées » montrent l'icône d'oxytone et son nom au lieu de l'icône
//! générique d'un programme.

fn main() {
    println!("cargo:rerun-if-changed=../../packaging/windows/oxytone.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon("../../packaging/windows/oxytone.ico")
        .set("FileDescription", "oxytone")
        .set("ProductName", "oxytone")
        .set("CompanyName", "oxytone");
    if let Err(e) = resource.compile() {
        // Sans le compilateur de ressources (SDK Windows), l'exe reste valide,
        // simplement sans icône.
        println!("cargo:warning=icône d'oxytone.exe non intégrée : {e}");
    }
}
