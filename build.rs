fn main() {
    println!("cargo:rerun-if-changed=assets/shike.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/shike.ico")
            .set("ProductName", "拾刻")
            .set("FileDescription", "拾刻 · 直播奖励定时领取")
            .set("LegalCopyright", "本地个人工具");
        res.compile().expect("生成应用图标失败");
    }
}
