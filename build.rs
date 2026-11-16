//! Script Cargo: origine publique autorisée et ressources natives Windows.
//! Ce fichier s'exécute pendant la compilation, jamais sur le PC du joueur.
//! Les fichiers intermédiaires restent dans OUT_DIR, les sources restent intactes.

fn main() {
    // En monorepo, la configuration centrale fait autorité. Dans l'export public,
    // companion.toml est autonome et ne contient que le domaine public.
    let monorepo = std::path::Path::new("../deployment/domain.toml");
    let path = if monorepo.is_file() {
        monorepo
    } else {
        std::path::Path::new("companion.toml")
    };
    println!("cargo:rerun-if-changed=companion.toml");
    println!("cargo:rerun-if-changed={}", path.display());
    let settings: toml::Value = std::fs::read_to_string(path).unwrap().parse().unwrap();
    let main = settings["domain"].as_str().unwrap();
    assert!(
        main.contains('.')
            && main
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b".-".contains(&c))
    );
    // env! dans api.rs incorpore une origine HTTPS exacte, sans secret de serveur.
    println!("cargo:rustc-env=UHC_LINK_SITE=https://{main}");
    println!("cargo:rerun-if-changed=resources/app.manifest");
    println!("cargo:rerun-if-changed=resources/app.rc");
    println!("cargo:rerun-if-changed=resources/app.ico");
    println!("cargo:rerun-if-env-changed=UHC_LINK_WINDRES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Le pipeline utilise MinGW, refuser un autre ABI plutôt que produire
        // des ressources incompatibles ou un binaire sans métadonnées.
        assert_eq!(
            std::env::var("CARGO_CFG_TARGET_ENV").as_deref(),
            Ok("gnu"),
            "Utiliser le target x86_64-pc-windows-gnu (MinGW et windres)"
        );
        let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
        // Windows VERSIONINFO demande quatre nombres, Cargo en expose trois.
        let version = format!("{}.0", std::env::var("CARGO_PKG_VERSION").unwrap());
        for name in ["app.manifest", "app.rc"] {
            let source = std::fs::read_to_string(format!("resources/{name}")).unwrap();
            std::fs::write(
                out.join(name),
                source
                    .replace("@VERSION_DOT@", &version)
                    .replace("@VERSION_COMMA@", &version.replace('.', ",")),
            )
            .unwrap();
        }
        std::fs::copy("resources/app.ico", out.join("app.ico"))
            .expect("L’icône Windows resources/app.ico est requise");
        let compiler = std::env::var("UHC_LINK_WINDRES")
            .unwrap_or_else(|_| "x86_64-w64-mingw32-windres".into());
        // windres transforme manifeste, icône et VERSIONINFO en objet COFF,
        // ensuite lié seulement au binaire Windows, pas à la bibliothèque de test.
        let status = std::process::Command::new(compiler)
            .current_dir(&out)
            .args([
                "--input",
                "app.rc",
                "--output",
                "app-res.o",
                "--output-format",
                "coff",
            ])
            .status()
            .expect("Le compilateur de ressources Windows windres est requis");
        assert!(
            status.success(),
            "Compilation des ressources Windows impossible"
        );
        println!(
            "cargo:rustc-link-arg-bin=UHC-Link-Raccourcis={}",
            out.join("app-res.o").display()
        );
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
            // Éviter l'horodatage variable du linker PE. Une signature réalisée
            // ensuite peut quand même faire varier l'empreinte du fichier signé.
            println!("cargo:rustc-link-arg-bin=UHC-Link-Raccourcis=-Wl,--no-insert-timestamp");
        }
    }
}
