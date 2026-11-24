//! Compiles the Slint user interface.

fn main() {
    // The widget style is fixed at compile time. Fluent looks the same on every
    // desktop; `SLINT_STYLE=cosmic cargo run -p konectl-gui` shows another one.
    println!("cargo:rerun-if-env-changed=SLINT_STYLE");
    let style: String = std::env::var("SLINT_STYLE").unwrap_or_else(|_| "fluent".to_owned());
    let config = slint_build::CompilerConfiguration::new()
        .with_edition((1, 19))
        .with_style(style);
    slint_build::compile_with_config("ui/app-window.slint", config).expect("the UI compiles");
}
