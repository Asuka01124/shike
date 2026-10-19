use crate::input::{self, TextInput};
use gpui::{prelude::*, *};
use shike::{
    api::Api,
    browser,
    engine::{self, Event},
    store::{self, Store, Task, TaskState},
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};
const INK: u32 = 0x202023;
const MUTED: u32 = 0x76767f;
const BORDER: u32 = 0xebebee;
const BLUE: u32 = 0x3d6af2;
actions!(shike_ui, [EscapeDialog, NextField]);
enum LoginEvent {
    Status(String),
    Done(Result<(String, String, u64), String>),
}
struct Tray {
    _icon: TrayIcon,
    open: tray_icon::menu::MenuId,
    quit: tray_icon::menu::MenuId,
}
fn make_tray() -> Option<Tray> {
    let menu = Menu::new();
    let open = MenuItem::new("打开拾刻", true, None);
    let quit = MenuItem::new("退出软件", true, None);
    menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit])
        .ok()?;
    let mut pixels = vec![0u8; 32 * 32 * 4];
    for y in 0..32usize {
        for x in 0..32usize {
            let i = (y * 32 + x) * 4;
            let hand = (15..=17).contains(&x) && (9..=17).contains(&y)
                || (15..=17).contains(&y) && (15..=23).contains(&x);
            let color = if hand {
                [61, 106, 242, 255]
            } else if (3..29).contains(&x) && (3..29).contains(&y) {
                [235, 241, 255, 255]
            } else {
                [0, 0, 0, 0]
            };
            pixels[i..i + 4].copy_from_slice(&color);
        }
    }
    let tray = TrayIconBuilder::new()
        .with_tooltip("拾刻 · 定时领取")
        .with_icon(Icon::from_rgba(pixels, 32, 32).ok()?)
        .with_menu(Box::new(menu))
        .build()
        .ok()?;
    Some(Tray {
        _icon: tray,
        open: open.id().clone(),
        quit: quit.id().clone(),
    })
}
pub fn hwnd(window: &Window) -> usize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match HasWindowHandle::window_handle(window)
        .ok()
        .map(|h| h.as_raw())
    {
        Some(RawWindowHandle::Win32(h)) => h.hwnd.get() as usize,
        _ => 0,
    }
}
pub fn hide_native(handle: usize) {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::{
            Foundation::HWND,
            UI::WindowsAndMessaging::{SW_HIDE, ShowWindow},
        };
        let _ = ShowWindow(HWND(handle as *mut _), SW_HIDE);
    }
}
pub fn show_native(handle: usize) {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::{
            Foundation::HWND,
            UI::WindowsAndMessaging::{SW_RESTORE, SetForegroundWindow, ShowWindow},
        };
        let h = HWND(handle as *mut _);
        let _ = ShowWindow(h, SW_RESTORE);
        let _ = SetForegroundWindow(h);
    }
}
fn set_awake(active: bool) {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Power::{
            ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
        };
        let _ = SetThreadExecutionState(if active {
            ES_CONTINUOUS | ES_SYSTEM_REQUIRED
        } else {
            ES_CONTINUOUS
        });
    }
}
struct HoverMotion {
    from: f32,
    target: f32,
    started: Instant,
}
impl HoverMotion {
    fn offset(&self) -> f32 {
        let progress = (self.started.elapsed().as_secs_f32() / 0.14).min(1.);
        let eased = 1. - (1. - progress).powi(3);
        self.from + (self.target - self.from) * eased
    }
}

#[derive(IntoElement)]
struct LiftButton {
    id: ElementId,
    frame: Stateful<Div>,
    surface: Div,
}
impl LiftButton {
    fn new(id: impl Into<ElementId>, surface: Div) -> Self {
        let id = id.into();
        Self {
            frame: div().id(id.clone()).cursor_pointer(),
            id,
            surface,
        }
    }
    fn on_click(mut self, listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.frame = self.frame.on_click(listener);
        self
    }
}
impl RenderOnce for LiftButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let motion = window.with_element_namespace(self.id, |window| {
            window.use_keyed_state("hover-motion", cx, |_, _| HoverMotion {
                from: 0.,
                target: 0.,
                started: Instant::now(),
            })
        });
        let state = motion.read(cx);
        let offset = state.offset();
        if (offset - state.target).abs() > 0.001 {
            window.request_animation_frame();
        }
        // 只移动按钮表面，点击区域和周围布局保持稳定。
        self.frame
            .on_hover(move |hovered, _, cx| {
                motion.update(cx, |state, cx| {
                    state.from = state.offset();
                    state.target = if *hovered { -2. } else { 0. };
                    state.started = Instant::now();
                    cx.notify();
                });
            })
            .child(self.surface.relative().top(px(offset)))
    }
}
fn text_button(id: impl Into<ElementId>, text: &str) -> LiftButton {
    LiftButton::new(
        id,
        div()
            .px_2()
            .py_2()
            .rounded(px(6.))
            .text_size(px(12.))
            .text_color(rgb(MUTED))
            .child(text.to_string()),
    )
}
fn card_button(id: impl Into<ElementId>, text: &str) -> LiftButton {
    LiftButton::new(
        id,
        div()
            .px(px(19.))
            .py(px(12.))
            .rounded(px(10.))
            .border_1()
            .border_color(rgb(0xe7e8ec))
            .bg(rgb(0xeff0f3))
            .text_color(rgb(0x555660))
            .text_size(px(13.))
            .font_weight(FontWeight::MEDIUM)
            .child(text.to_string()),
    )
}

fn small_card_button(id: impl Into<ElementId>, text: &str) -> LiftButton {
    let mut button = card_button(id, text);
    button.surface = button
        .surface
        .px(px(12.))
        .py(px(8.))
        .rounded(px(8.))
        .text_size(px(12.));
    button
}

#[derive(IntoElement)]
struct AccountActions {
    login: Stateful<Div>,
    quit: Stateful<Div>,
}
impl RenderOnce for AccountActions {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let motion = window.use_keyed_state("account-slider-motion", cx, |_, _| HoverMotion {
            from: 0.,
            target: 0.,
            started: Instant::now(),
        });
        let state = motion.read(cx);
        let offset = state.offset();
        if (offset - state.target).abs() > 0.001 {
            window.request_animation_frame();
        }
        let login_motion = motion.clone();
        let quit_motion = motion.clone();
        div()
            .id("account-slider")
            .relative()
            .flex()
            .p(px(4.))
            .w(px(200.))
            .h(px(42.))
            .rounded(px(10.))
            .bg(rgb(0xeff0f3))
            .on_hover(move |hovered, _, cx| {
                if !*hovered {
                    motion.update(cx, |state, cx| {
                        state.from = state.offset();
                        state.target = 0.;
                        state.started = Instant::now();
                        cx.notify();
                    });
                }
            })
            .child(
                div()
                    .absolute()
                    .left(px(4. + offset))
                    .top(px(4.))
                    .w(px(96.))
                    .h(px(34.))
                    .rounded(px(7.))
                    .border_1()
                    .border_color(rgb(0xe3e5e9))
                    .bg(rgb(0xfafafb)),
            )
            .child(self.login.on_hover(move |hovered, _, cx| {
                if *hovered {
                    login_motion.update(cx, |state, cx| {
                        state.from = state.offset();
                        state.target = 0.;
                        state.started = Instant::now();
                        cx.notify();
                    });
                }
            }))
            .child(self.quit.on_hover(move |hovered, _, cx| {
                if *hovered {
                    quit_motion.update(cx, |state, cx| {
                        state.from = state.offset();
                        state.target = 96.;
                        state.started = Instant::now();
                        cx.notify();
                    });
                }
            }))
    }
}
fn account_actions(
    label: &str,
    login: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    quit: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> LiftButton {
    let segment = || {
        div()
            .w(px(96.))
            .h(px(34.))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .text_size(px(12.))
            .text_color(rgb(0x555660))
    };
    LiftButton::new(
        "account-actions",
        div().child(AccountActions {
            login: segment()
                .id("login")
                .child(label.to_string())
                .on_click(login),
            quit: segment().id("quit").child("退出登录").on_click(quit),
        }),
    )
}

fn time_button(id: &'static str, text: &str, selected: bool) -> LiftButton {
    LiftButton::new(
        id,
        div()
            .px(px(15.))
            .py(px(10.))
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(if selected { 0xb8bbc4 } else { 0xe7e8ec }))
            .bg(rgb(0xeff0f3))
            .text_color(rgb(if selected { 0x41434c } else { 0x555660 }))
            .text_size(px(13.))
            .child(text.to_string()),
    )
}

#[derive(Clone, Copy, PartialEq)]
enum TimeChoice {
    Midnight,
    Now,
    Custom,
}

// 将底层错误转成用户可以理解、可以采取行动的提示，原始结果仍保留在记录中。
fn user_message(message: &str) -> String {
    let message = message.trim();
    let message = message
        .rsplit_once(" · ")
        .filter(|(_, code)| code.parse::<i64>().is_ok())
        .map_or(message, |(text, _)| text);
    let message = message
        .strip_suffix('）')
        .and_then(|text| text.rsplit_once('（'))
        .filter(|(_, code)| code.parse::<i64>().is_ok())
        .map_or(message, |(text, _)| text);
    if message.contains("核对领奖记录") {
        "暂时无法确认领取结果，请先打开领取页面核对，再决定是否重试。".into()
    } else if message.contains("登录已过期") || message.contains("登录信息不完整") {
        "登录已失效，请重新登录B站。".into()
    } else if message.contains("网页验证") || message.contains("验证码") {
        "需要完成验证，请打开领取页面，按提示操作后再试。".into()
    } else if message.contains("限流") || message.contains("请求受限") {
        "B站暂时限制了领取，请稍后再试。".into()
    } else if message.contains("暂无领取资格") {
        "暂未满足领取条件，请打开领取页面查看要求。".into()
    } else if message.contains("错过执行时间") {
        "已错过领取时间，请重新选择时间。".into()
    } else if message.contains("本轮结束") {
        "这次未能领取，可以重新选择时间再试。".into()
    } else if message.starts_with("读取任务失败") {
        "暂时无法读取已保存的奖励，请重新打开拾刻。".into()
    } else if message.starts_with("保存失败") {
        "未能保存更改，请检查程序所在文件夹是否可以写入。".into()
    } else if message.contains("Chrome或Edge") {
        "请先安装 Chrome 或 Edge 浏览器，再登录B站。".into()
    } else if message.is_empty() {
        "暂时无法领取，请打开领取页面查看详情。".into()
    } else if message.contains("签名")
        || message.contains("HTTP")
        || message.contains("request")
        || message.contains("timed out")
        || message.contains("://")
        || (!message.is_empty() && message.is_ascii())
    {
        "暂时无法连接B站，请检查网络后再试。".into()
    } else {
        message.to_string()
    }
}

pub struct Desk {
    store: Store,
    dir: PathBuf,
    archive: bool,
    dialog: bool,
    editing: Option<u64>,
    link: Entity<TextInput>,
    time: Entity<TextInput>,
    time_choice: TimeChoice,
    link_error: String,
    time_error: String,
    notice: String,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    active: HashMap<u64, Arc<AtomicBool>>,
    session_api: Option<Api>,
    login_rx: Option<Receiver<LoginEvent>>,
    login_cancel: Option<Arc<AtomicBool>>,
    tray: Option<Tray>,
    handle: usize,
    demo: bool,
    power_busy: bool,
    stopping: HashSet<u64>,
}
impl Desk {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, demo: bool) -> Self {
        input::bind_keys(cx);
        cx.bind_keys([
            KeyBinding::new("escape", EscapeDialog, Some("Shike")),
            KeyBinding::new("tab", NextField, Some("Shike")),
        ]);
        let dir = store::data_dir();
        let (mut saved, error) = match Store::load(&dir) {
            Ok(s) => (s, String::new()),
            Err(e) => (Store::default(), format!("读取任务失败：{e}")),
        };
        if demo {
            saved = Store {
                user_name: "演示账号".into(),
                user_id: 1,
                cookie: "demo-session".into(),
                ..Default::default()
            };
            for (i, title) in [
                "累计3天 · 嵌晶玉 × 500",
                "累计5天 · 嵌晶玉 × 500",
                "新人开播参与奖 · 嵌晶玉 × 250",
            ]
            .iter()
            .enumerate()
            {
                saved.add(&format!("https://www.bilibili.com/blackboard/era/award-exchange.html?task_id=demo{i}"),store::parse_time(&store::next_midnight()).unwrap()).ok();
                saved.tasks[i].title = title.to_string();
                saved.tasks[i].state = TaskState::Waiting;
                saved.tasks[i].message = "等待零点".into();
            }
            saved.archive(3, "领取成功 · 已发送站内信".into(), None);
            saved.tasks[1].owner_id = 2;
            saved.tasks[1].owner_name = "另一个账号".into();
            saved.tasks[1].state = TaskState::WaitingLogin;
            saved.tasks[0].state = TaskState::CoolingDown;
            saved.tasks[0].message = "暂时受限，稍后自动继续".into();
        }
        let (tx, rx) = mpsc::channel();
        let tray = make_tray();
        let can_hide = tray.is_some();
        window.set_window_title(if demo {
            "拾刻 · 界面预览"
        } else {
            "拾刻"
        });
        window.on_window_should_close(cx, move |w, cx| {
            if can_hide {
                hide_native(hwnd(w));
                false
            } else {
                cx.quit();
                true
            }
        });
        cx.spawn(async move |this, cx| {
            loop {
                smol::Timer::after(Duration::from_millis(200)).await;
                if this.update(cx, |v, cx| v.tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        Self {
            store: saved,
            dir,
            archive: false,
            dialog: false,
            editing: None,
            link: cx.new(|cx| TextInput::new("", "粘贴奖励领取页链接", cx)),
            time: cx.new(|cx| TextInput::new(&store::next_midnight(), "年-月-日 时:分", cx)),
            time_choice: TimeChoice::Midnight,
            link_error: String::new(),
            time_error: String::new(),
            notice: error,
            tx,
            rx,
            active: HashMap::new(),
            session_api: None,
            login_rx: None,
            login_cancel: None,
            tray,
            handle: hwnd(window),
            demo,
            power_busy: false,
            stopping: HashSet::new(),
        }
    }
    fn save(&mut self) {
        if !self.demo {
            if let Err(e) = self.store.save(&self.dir) {
                self.notice = format!("保存失败：{e}");
            }
        }
    }
    fn login(&mut self, cx: &mut Context<Self>) {
        if self.demo {
            self.notice = "演示模式 · 不连接账号".into();
            cx.notify();
            return;
        }
        if self.login_cancel.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let flag = Arc::new(AtomicBool::new(false));
        self.login_cancel = Some(flag.clone());
        self.login_rx = Some(rx);
        self.notice = "请在浏览器中登录".into();
        let dir = self.dir.clone();
        std::thread::spawn(move || {
            let reporter = tx.clone();
            let result = browser::login(&dir, flag, move |s| {
                let _ = reporter.send(LoginEvent::Status(s));
            })
            .map_err(|e| e.to_string());
            let _ = tx.send(LoginEvent::Done(result));
        });
        cx.notify();
    }
    fn tick(&mut self, cx: &mut Context<Self>) {
        let logins: Vec<_> = self
            .login_rx
            .as_ref()
            .map(|r| r.try_iter().collect())
            .unwrap_or_default();
        for e in logins {
            match e {
                LoginEvent::Status(s) => {
                    if !self
                        .login_cancel
                        .as_ref()
                        .is_some_and(|c| c.load(Ordering::Acquire))
                    {
                        self.notice = s;
                    }
                }
                LoginEvent::Done(r) => {
                    let canceled = self
                        .login_cancel
                        .as_ref()
                        .is_some_and(|c| c.load(Ordering::Acquire));
                    self.login_cancel = None;
                    self.login_rx = None;
                    if canceled {
                        continue;
                    }
                    match r {
                        Ok((c, n, m)) => {
                            self.stop_session_workers();
                            self.store.sign_in(c, n, m, chrono::Utc::now().timestamp());
                            self.notice = "登录成功".into();
                            self.save();
                        }
                        Err(e) => self.notice = e,
                    }
                }
            }
        }
        let events: Vec<_> = self.rx.try_iter().collect();
        let mut changed = false;
        for e in events {
            let id = e.task_id();
            if matches!(e, Event::Finished(_)) {
                self.active.remove(&id);
                self.stopping.remove(&id);
                continue;
            }
            changed |= self.store.apply_event(&e, self.stopping.contains(&id));
        }
        if changed {
            self.save();
        }
        if !self.demo && !self.store.cookie.is_empty() {
            let ready: Vec<_> = self
                .store
                .tasks
                .iter()
                .filter(|t| {
                    matches!(t.state, TaskState::Pending | TaskState::CoolingDown)
                        && self.store.can_run(t)
                        && !self.active.contains_key(&t.id)
                })
                .cloned()
                .collect();
            for t in ready {
                let id = t.id;
                if self.session_api.is_none() {
                    match Api::new(&self.store.cookie) {
                        Ok(api) => self.session_api = Some(api),
                        Err(error) => {
                            self.store
                                .apply_event(&Event::Failed(id, error.to_string()), false);
                            self.save();
                            continue;
                        }
                    }
                }
                let api = self.session_api.as_ref().unwrap().clone();
                let until = self
                    .store
                    .tasks
                    .iter()
                    .filter(|t| t.owner_id == self.store.user_id)
                    .map(|t| t.prep_retry_at)
                    .max()
                    .unwrap_or(0);
                api.restore_preparation_deadline(until);
                if let Some(t) = self.store.tasks.iter_mut().find(|t| t.id == id) {
                    if t.state != TaskState::CoolingDown {
                        t.state = TaskState::Preparing;
                        t.message = "准备奖励信息".into();
                    }
                }
                self.active
                    .insert(id, engine::start_with_api(t, api, self.tx.clone()));
                self.save();
            }
        }
        let busy = !self.active.is_empty();
        if busy != self.power_busy {
            self.power_busy = busy;
            set_awake(busy);
        }
        if let Some(tray) = &self.tray {
            let (mut show, mut quit) = (false, false);
            while let Ok(e) = MenuEvent::receiver().try_recv() {
                if e.id == tray.open {
                    show = true;
                }
                if e.id == tray.quit {
                    quit = true;
                }
            }
            while let Ok(e) = TrayIconEvent::receiver().try_recv() {
                if matches!(e, TrayIconEvent::DoubleClick { .. }) {
                    show = true;
                }
            }
            if show {
                show_native(self.handle);
            }
            if quit {
                self.quit(cx);
                return;
            }
        }
        cx.notify();
    }
    fn stop_session_workers(&mut self) {
        self.session_api = None;
        for (id, cancel) in &self.active {
            cancel.store(true, Ordering::Release);
            self.stopping.insert(*id);
        }
    }
    fn logout(&mut self, cx: &mut Context<Self>) {
        self.stop_session_workers();
        if let Some(cancel) = &self.login_cancel {
            cancel.store(true, Ordering::Release);
        }
        self.store.sign_out();
        self.notice = "已退出登录，领取安排和记录已保留。".into();
        self.save();
        cx.notify();
    }
    fn quit(&mut self, cx: &mut Context<Self>) {
        self.save();
        for c in self.active.values() {
            c.store(true, Ordering::Release);
        }
        if let Some(c) = &self.login_cancel {
            c.store(true, Ordering::Release);
        }
        set_awake(false);
        cx.quit();
    }
    fn new_task(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        self.editing = None;
        self.time_choice = TimeChoice::Midnight;
        self.link_error.clear();
        self.time_error.clear();
        self.link.update(cx, |v, cx| v.set_text("", cx));
        self.time
            .update(cx, |v, cx| v.set_text(&store::next_midnight(), cx));
        self.dialog = true;
        self.notice.clear();
        w.focus(&self.link.focus_handle(cx));
        cx.notify();
    }
    fn edit(&mut self, id: u64, w: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = self.store.tasks.iter().find(|t| t.id == id).cloned() {
            if t.state == TaskState::Archived {
                self.notice = "这份奖励已经领取，请在领取记录中查看。".into();
                cx.notify();
                return;
            }
            if self.active.contains_key(&id) && t.target <= chrono::Utc::now().timestamp() {
                self.notice = "正在确认领取结果，请稍后再修改。".into();
                cx.notify();
                return;
            }
            self.editing = Some(id);
            self.time_choice = TimeChoice::Custom;
            self.link_error.clear();
            self.time_error.clear();
            self.link.update(cx, |v, cx| v.set_text(&t.url, cx));
            let time = if t.target <= chrono::Utc::now().timestamp() {
                store::next_midnight()
            } else {
                store::china_time(t.target, "%Y-%m-%d %H:%M:%S")
            };
            self.time.update(cx, |v, cx| v.set_text(&time, cx));
            self.dialog = true;
            self.notice.clear();
            w.focus(&self.time.focus_handle(cx));
            cx.notify();
        }
    }
    fn commit(&mut self, cx: &mut Context<Self>) {
        let link = self.link.read(cx).content.to_string();
        self.link_error.clear();
        self.time_error.clear();
        if let Some(task) = self
            .editing
            .and_then(|id| self.store.tasks.iter().find(|t| t.id == id))
        {
            if task.state == TaskState::Archived {
                self.link_error = "这份奖励已经领取，请在领取记录中查看。".into();
                cx.notify();
                return;
            }
            if self.active.contains_key(&task.id) && task.target <= chrono::Utc::now().timestamp() {
                self.link_error = "正在确认领取结果，请稍后再修改。".into();
                cx.notify();
                return;
            }
        }
        let tid = match store::parse_task_id(&link) {
            Ok(id) => id,
            Err(e) => {
                self.link_error = user_message(&e.to_string());
                cx.notify();
                return;
            }
        };
        let owner = self
            .editing
            .and_then(|id| self.store.tasks.iter().find(|t| t.id == id))
            .map(|t| t.owner_id)
            .unwrap_or(self.store.user_id);
        if self.editing.is_none() && !self.store.is_logged_in() {
            self.link_error = "请先登录B站，再添加奖励。".into();
            cx.notify();
            return;
        }
        if self.time_choice == TimeChoice::Now
            && (!self.store.is_logged_in() || owner != self.store.user_id)
        {
            self.time_error = "请先登录这份奖励所属的账号，再选择现在领取。".into();
            cx.notify();
            return;
        }
        if let Some(old) = self.store.tasks.iter().find(|t| {
            t.task_id == tid
                && t.owner_id == owner
                && (Some(t.id) != self.editing || t.state == TaskState::Archived)
        }) {
            self.link_error = if old.state == TaskState::Archived {
                "这份奖励已经领过了，可在领取记录中查看。"
            } else {
                "已添加这份奖励，可在待领取中修改时间。"
            }
            .into();
            cx.notify();
            return;
        }
        let now = chrono::Utc::now().timestamp();
        let target = match self.time_choice {
            TimeChoice::Midnight => store::parse_time(&store::next_midnight()).unwrap(),
            TimeChoice::Now => now + 1,
            TimeChoice::Custom => match store::parse_time(&self.time.read(cx).content) {
                Ok(target) => target,
                Err(_) => {
                    self.time_error = format!(
                        "请按“{}”填写时间。",
                        store::china_time(now + 3600, "%Y-%m-%d %H:%M")
                    );
                    cx.notify();
                    return;
                }
            },
        };
        if target < now - 3 {
            self.time_error = "这个时间已经过去了，请选择之后的时间。".into();
            cx.notify();
            return;
        }
        if let Some(id) = self.editing {
            if let Some(c) = self.active.get(&id) {
                c.store(true, Ordering::Release);
                self.stopping.insert(id);
            }
            if let Some(t) = self.store.tasks.iter_mut().find(|t| t.id == id) {
                t.url = link;
                if t.task_id != tid {
                    t.prepared = None;
                }
                t.task_id = tid;
                t.target = target;
                t.state =
                    if owner != 0 && owner == self.store.user_id && !self.store.cookie.is_empty() {
                        TaskState::Pending
                    } else {
                        TaskState::WaitingLogin
                    };
                t.attempts = [0; 2];
                t.prep_failures = 0;
                t.message = "等待准备".into();
            }
        } else if let Err(e) = self.store.add(&link, target) {
            self.link_error = user_message(&e.to_string());
            cx.notify();
            return;
        }
        self.dialog = false;
        self.archive = false;
        self.notice.clear();
        self.save();
        cx.notify();
    }
    fn toggle(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(task) = self.store.tasks.iter().find(|t| t.id == id).cloned() else {
            return;
        };
        if !self.store.can_run(&task) {
            self.notice = format!("请先登录“{}”，再继续领取。", task.owner_name);
            cx.notify();
            return;
        }
        if let Some(cancel) = self.active.get(&id) {
            cancel.store(true, Ordering::Release);
            self.stopping.insert(id);
        }
        if let Some(t) = self.store.tasks.iter_mut().find(|t| t.id == id) {
            if task.state == TaskState::Paused {
                if t.target <= chrono::Utc::now().timestamp() {
                    t.state = TaskState::Failed;
                    t.message = "已错过执行时间，请重新设置".into();
                } else {
                    t.state = TaskState::Pending;
                    t.message = "等待准备".into();
                }
            } else {
                t.state = TaskState::Paused;
                t.message = "已暂停".into();
            }
        }
        self.save();
        cx.notify();
    }
    fn escape(&mut self, _: &EscapeDialog, _: &mut Window, cx: &mut Context<Self>) {
        self.dialog = false;
        cx.notify();
    }
    fn next_field(&mut self, _: &NextField, w: &mut Window, cx: &mut Context<Self>) {
        if self.dialog && self.time_choice == TimeChoice::Custom {
            let f = if self.link.focus_handle(cx).is_focused(w) {
                self.time.focus_handle(cx)
            } else {
                self.link.focus_handle(cx)
            };
            w.focus(&f);
        }
    }
    fn row(&self, t: &Task, cx: &mut Context<Self>) -> Stateful<Div> {
        let id = t.id;
        let archived = t.state == TaskState::Archived;
        let failed = t.state == TaskState::Failed;
        let running = self.active.contains_key(&id) && !self.stopping.contains(&id);
        let paused = t.state == TaskState::Paused;
        let (url, cdk) = (t.url.clone(), t.cdkey.clone());
        let (label, color) = match t.state {
            TaskState::Pending | TaskState::Preparing
                if self.store.cookie.is_empty() && !self.demo =>
            {
                ("等待登录", MUTED)
            }
            TaskState::WaitingLogin => (
                if self.store.is_logged_in() {
                    "等待登录该账号"
                } else {
                    "等待登录"
                },
                MUTED,
            ),
            TaskState::Pending | TaskState::Preparing => ("准备中", MUTED),
            TaskState::Waiting => ("已安排", BLUE),
            TaskState::CoolingDown => ("等待恢复", MUTED),
            TaskState::Running => ("领取中", BLUE),
            TaskState::Paused => ("已暂停", MUTED),
            TaskState::Failed => ("待处理", 0xb76a39),
            TaskState::Archived => ("已领取", 0x288169),
        };
        let title = if t.title == "新领取任务" {
            "待领取奖励"
        } else {
            &t.title
        };
        let detail = if archived {
            format!(
                "{} 领取",
                store::china_time(t.archived_at.unwrap_or(t.target), "%m月%d日 %H:%M")
            )
        } else if t.state == TaskState::Running {
            "正在为你领取，请稍候。".into()
        } else if failed {
            format!(
                "原定 {} 领取",
                store::china_time(t.target, "%m月%d日 %H:%M")
            )
        } else {
            format!("{} 开始领取", store::china_time(t.target, "%m月%d日 %H:%M"))
        };
        div()
            .id(("task", id as usize))
            .flex()
            .items_center()
            .gap_4()
            .px(px(24.))
            .py(px(22.))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .min_w_0()
                                    .text_size(px(15.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_ellipsis()
                                    .child(title.to_string()),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(px(12.))
                                    .text_color(rgb(color))
                                    .child(label),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(MUTED))
                            .text_ellipsis()
                            .child(format!(
                                "{} · {detail}",
                                if t.owner_name.is_empty() {
                                    "待确认账号"
                                } else {
                                    &t.owner_name
                                }
                            )),
                    )
                    .when(failed || t.state == TaskState::CoolingDown, |el| {
                        el.child(
                            div()
                                .text_size(px(12.))
                                .line_height(px(19.))
                                .text_color(rgb(if failed { 0xb76a39 } else { MUTED }))
                                .child(if failed {
                                    user_message(&t.message)
                                } else {
                                    "暂时受限，稍后自动继续。领取时间已保留。".into()
                                }),
                        )
                    }),
            )
            .when(!archived, |el| {
                el.child(
                    small_card_button(
                        ("edit", id as usize),
                        if failed { "重新安排" } else { "修改" },
                    )
                    .on_click(cx.listener(move |v, _, w, cx| v.edit(id, w, cx))),
                )
                .when(
                    !failed && (running || paused || t.state == TaskState::CoolingDown),
                    |el| {
                        el.child(
                            small_card_button(
                                ("toggle", id as usize),
                                if paused {
                                    "继续领取"
                                } else {
                                    "暂停领取"
                                },
                            )
                            .on_click(cx.listener(move |v, _, _, cx| v.toggle(id, cx))),
                        )
                    },
                )
                .when(failed, |el| {
                    let link = t.url.clone();
                    el.child(
                        text_button(("review", id as usize), "打开领取页")
                            .on_click(move |_, _, cx| cx.open_url(&link)),
                    )
                })
            })
            .when(archived, |el| {
                el.child(
                    text_button(
                        ("record", id as usize),
                        if cdk.is_some() {
                            "复制兑换码"
                        } else {
                            "打开领取页"
                        },
                    )
                    .on_click(cx.listener(move |v, _, _, cx| {
                        if let Some(c) = &cdk {
                            cx.write_to_clipboard(ClipboardItem::new_string(c.clone()));
                            v.notice = "兑换码已复制。".into();
                            cx.notify();
                        } else {
                            cx.open_url(&url);
                        }
                    })),
                )
            })
    }
    fn modal(&self, cx: &mut Context<Self>) -> Div {
        let editing = self.editing.is_some();
        let task_owner = self
            .editing
            .and_then(|id| self.store.tasks.iter().find(|t| t.id == id));
        let login_required = if let Some(task) = task_owner {
            self.time_choice == TimeChoice::Now && !self.store.can_run(task)
        } else {
            !self.store.is_logged_in()
        };
        let time_note = match self.time_choice {
            TimeChoice::Midnight => format!(
                "{} · 北京时间",
                store::china_time(
                    store::parse_time(&store::next_midnight()).unwrap(),
                    "%m月%d日 00:00"
                )
            ),
            TimeChoice::Now if login_required => {
                if self.notice.is_empty() {
                    "请先登录B站，登录后即可开始领取。".into()
                } else {
                    user_message(&self.notice)
                }
            }
            TimeChoice::Now => "确认后立即开始领取".into(),
            TimeChoice::Custom => format!(
                "北京时间，例如 {}",
                store::china_time(chrono::Utc::now().timestamp() + 3600, "%Y-%m-%d %H:%M")
            ),
        };
        div()
            .absolute()
            .inset_0()
            .bg(rgba(0xe8ebf2b8))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .w(px(552.))
                    .bg(rgb(0xffffff))
                    .rounded(px(18.))
                    .border_1()
                    .border_color(rgb(0xe5e5e9))
                    .p(px(32.))
                    .flex()
                    .flex_col()
                    .gap(px(26.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(23.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(if editing {
                                        "修改领取安排"
                                    } else {
                                        "添加奖励"
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(rgb(MUTED))
                                    .child(format!(
                                        "所属账号：{}",
                                        task_owner
                                            .map(|t| t.owner_name.as_str())
                                            .filter(|s| !s.is_empty())
                                            .unwrap_or(if self.store.is_logged_in() {
                                                &self.store.user_name
                                            } else {
                                                "登录后确定"
                                            })
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("奖励链接"),
                            )
                            .child(self.link.clone())
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .line_height(px(18.))
                                    .text_color(rgb(if self.link_error.is_empty() {
                                        MUTED
                                    } else {
                                        0xb76a39
                                    }))
                                    .child(if self.link_error.is_empty() {
                                        "打开活动中的具体奖励，复制领取页面的链接。".to_string()
                                    } else {
                                        self.link_error.clone()
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("什么时候领取"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(
                                        time_button(
                                            "midnight",
                                            "明天零点",
                                            self.time_choice == TimeChoice::Midnight,
                                        )
                                        .on_click(
                                            cx.listener(|v, _, _, cx| {
                                                v.time_choice = TimeChoice::Midnight;
                                                v.time_error.clear();
                                                cx.notify();
                                            }),
                                        ),
                                    )
                                    .child(
                                        time_button(
                                            "now",
                                            "现在领取",
                                            self.time_choice == TimeChoice::Now,
                                        )
                                        .on_click(
                                            cx.listener(|v, _, _, cx| {
                                                v.time_choice = TimeChoice::Now;
                                                v.time_error.clear();
                                                cx.notify();
                                            }),
                                        ),
                                    )
                                    .child(
                                        time_button(
                                            "custom",
                                            "自选时间",
                                            self.time_choice == TimeChoice::Custom,
                                        )
                                        .on_click(
                                            cx.listener(|v, _, w, cx| {
                                                v.time_choice = TimeChoice::Custom;
                                                v.time_error.clear();
                                                w.focus(&v.time.focus_handle(cx));
                                                cx.notify();
                                            }),
                                        ),
                                    ),
                            )
                            .when(self.time_choice == TimeChoice::Custom, |el| {
                                el.child(self.time.clone())
                            })
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .line_height(px(18.))
                                    .text_color(rgb(if self.time_error.is_empty() {
                                        MUTED
                                    } else {
                                        0xb76a39
                                    }))
                                    .child(if self.time_error.is_empty() {
                                        time_note
                                    } else {
                                        self.time_error.clone()
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .items_center()
                            .gap_3()
                            .pt_1()
                            .child(card_button("cancel", "取消").on_click(cx.listener(
                                |v, _, _, cx| {
                                    v.dialog = false;
                                    cx.notify();
                                },
                            )))
                            .child(
                                card_button(
                                    "commit",
                                    if login_required && self.login_cancel.is_some() {
                                        "请在浏览器登录"
                                    } else if login_required {
                                        "登录B站"
                                    } else if editing {
                                        "保存修改"
                                    } else if self.time_choice == TimeChoice::Now {
                                        "开始领取"
                                    } else {
                                        "添加奖励"
                                    },
                                )
                                .on_click(cx.listener(
                                    move |v, _, _, cx| {
                                        if login_required {
                                            v.login(cx);
                                        } else {
                                            v.commit(cx);
                                        }
                                    },
                                )),
                            ),
                    ),
            )
    }
}
impl Render for Desk {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pending = self
            .store
            .tasks
            .iter()
            .filter(|t| t.state != TaskState::Archived)
            .count();
        let archived = self.store.tasks.len() - pending;
        let now = chrono::Utc::now().timestamp();
        let logged = self.store.is_logged_in();
        let login_busy = self.login_cancel.is_some();
        let claiming = self
            .store
            .tasks
            .iter()
            .any(|t| t.state == TaskState::Running && self.store.can_run(t));
        let next = self
            .store
            .tasks
            .iter()
            .filter(|t| {
                matches!(
                    t.state,
                    TaskState::Pending
                        | TaskState::Waiting
                        | TaskState::Preparing
                        | TaskState::CoolingDown
                ) && self.store.can_run(t)
                    && t.target > now
            })
            .map(|t| t.target)
            .min()
            .filter(|_| logged && !claiming);
        let title = if let Some(target) = next {
            let seconds = target - now;
            format!(
                "{:02}:{:02}:{:02}",
                seconds / 3600,
                seconds % 3600 / 60,
                seconds % 60
            )
        } else if claiming {
            "正在领取奖励".into()
        } else {
            "定时领取奖励".into()
        };
        let subtitle = if let Some(target) = next {
            format!("{} 开始领取", store::china_time(target, "%m月%d日 %H:%M"))
        } else if claiming {
            "领取结果会保存在“领取记录”中。".into()
        } else if self
            .store
            .tasks
            .iter()
            .any(|t| t.state == TaskState::Failed)
        {
            "有奖励需要处理，请查看下方提示。".into()
        } else if self
            .store
            .tasks
            .iter()
            .any(|t| t.state == TaskState::Paused)
        {
            "领取已暂停，可在下方继续。".into()
        } else if self
            .store
            .tasks
            .iter()
            .any(|t| t.state == TaskState::WaitingLogin)
        {
            "登录对应账号后，未到期的奖励会自动恢复。".into()
        } else {
            "添加奖励链接，选好时间，到点自动领取。".into()
        };
        let mut rows: Vec<_> = self
            .store
            .tasks
            .iter()
            .filter(|t| (t.state == TaskState::Archived) == self.archive)
            .cloned()
            .collect();
        if self.archive {
            rows.sort_by_key(|t| std::cmp::Reverse(t.archived_at.unwrap_or(t.target)));
        } else {
            rows.sort_by_key(|t| t.target);
        }
        let empty = rows.is_empty();
        let content = div()
            .size_full()
            .flex()
            .flex_col()
            .px(px(38.))
            .pt(px(28.))
            .pb(px(20.))
            .gap(px(24.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .size(px(28.))
                                    .bg(rgb(0xf0f4ff))
                                    .rounded(px(8.)),
                            )
                            .child(
                                div()
                                    .text_size(px(19.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("拾刻"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .items_center()
                            .when(logged, |el| {
                                el.child(div().size(px(6.)).rounded_full().bg(rgb(0x6aaa91)))
                                    .child(
                                        div()
                                            .max_w(px(180.))
                                            .text_ellipsis()
                                            .text_size(px(12.))
                                            .text_color(rgb(MUTED))
                                            .child(self.store.user_name.clone()),
                                    )
                            })
                            .child(account_actions(
                                if login_busy {
                                    "正在登录"
                                } else if logged {
                                    "重新登录"
                                } else {
                                    "登录B站"
                                },
                                cx.listener(|v, _, _, cx| v.login(cx)),
                                cx.listener(|v, _, _, cx| v.logout(cx)),
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_6()
                    .min_h(px(134.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .when(next.is_some(), |el| {
                                el.child(
                                    div()
                                        .text_size(px(12.))
                                        .text_color(rgb(MUTED))
                                        .child("距离下一次领取"),
                                )
                            })
                            .child(
                                div()
                                    .text_size(px(if next.is_some() { 50. } else { 32. }))
                                    .line_height(px(if next.is_some() { 58. } else { 44. }))
                                    .font_weight(if next.is_some() {
                                        FontWeight::LIGHT
                                    } else {
                                        FontWeight::SEMIBOLD
                                    })
                                    .when(next.is_some(), |el| el.font_family("Segoe UI"))
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .line_height(px(20.))
                                    .text_color(rgb(MUTED))
                                    .child(subtitle),
                            ),
                    )
                    .child(
                        card_button("new-task", "＋  添加奖励")
                            .on_click(cx.listener(|v, _, w, cx| v.new_task(w, cx))),
                    ),
            )
            .when(!self.notice.is_empty(), |el| {
                el.child(
                    div()
                        .px(px(14.))
                        .py(px(11.))
                        .rounded(px(8.))
                        .bg(rgb(0xf0f4ff))
                        .text_size(px(13.))
                        .line_height(px(20.))
                        .text_color(rgb(0x52658e))
                        .child(user_message(&self.notice)),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .rounded(px(14.))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(0xffffff))
                    .overflow_hidden()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_6()
                            .px(px(24.))
                            .h(px(59.))
                            .flex_shrink_0()
                            .border_b_1()
                            .border_color(rgb(BORDER))
                            .child(
                                div()
                                    .id("pending-tab")
                                    .h_full()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .border_b_2()
                                    .border_color(rgb(if !self.archive {
                                        0xb7c9ff
                                    } else {
                                        0xffffff
                                    }))
                                    .cursor_pointer()
                                    .text_size(px(14.))
                                    .text_color(rgb(if !self.archive { INK } else { MUTED }))
                                    .child("待领取")
                                    .when(pending > 0, |el| el.child(format!("{pending}")))
                                    .on_click(cx.listener(|v, _, _, cx| {
                                        v.archive = false;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .id("archive-tab")
                                    .h_full()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .border_b_2()
                                    .border_color(rgb(if self.archive {
                                        0xb7c9ff
                                    } else {
                                        0xffffff
                                    }))
                                    .cursor_pointer()
                                    .text_size(px(14.))
                                    .text_color(rgb(if self.archive { INK } else { MUTED }))
                                    .child("领取记录")
                                    .when(archived > 0, |el| el.child(format!("{archived}")))
                                    .on_click(cx.listener(|v, _, _, cx| {
                                        v.archive = true;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .id("task-list")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .when(empty, |el| {
                                el.child(
                                    div()
                                        .flex_1()
                                        .min_h(px(160.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            div()
                                                .text_size(px(14.))
                                                .text_color(rgb(MUTED))
                                                .child("无任务"),
                                        ),
                                )
                            })
                            .children(rows.iter().map(|t| self.row(t, cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap_4()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .child("领取时请保持电脑开机、联网。")
                    .when(self.tray.is_some(), |el| el.child("关闭窗口后仍会继续等待")),
            );
        div()
            .relative()
            .key_context("Shike")
            .on_action(cx.listener(Self::escape))
            .on_action(cx.listener(Self::next_field))
            .size_full()
            .bg(rgb(0xfafafb))
            .font_family("Microsoft YaHei UI")
            .text_color(rgb(INK))
            .child(content)
            .when(self.dialog, |el| el.child(self.modal(cx)))
    }
}
