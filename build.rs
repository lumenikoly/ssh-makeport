fn main() {
    #[cfg(feature = "gui")]
    {
        println!("cargo:rerun-if-changed=assets");
        #[cfg(windows)]
        winresource::WindowsResource::new()
            .set_icon("assets/makeport.ico")
            .set("ProductName", "MakePort")
            .set(
                "FileDescription",
                "MakePort — SSH port forwarding",
            )
            .compile()
            .expect("Не удалось добавить иконку Windows");
        let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
        slint_build::compile_with_config("ui/app.slint", config)
            .expect("Не удалось скомпилировать интерфейс Slint");
    }
}
