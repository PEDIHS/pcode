//! Plugins: what Claude Code and Codex have installed, what their
//! marketplaces offer, and installing a plugin's skills and MCP servers into
//! Jcode. A local panel like the changelog; never a runtime session.
use super::*;
use jcode_desktop_model::plugins::{
    self, Ecosystem, InstallMode, InstalledPlugin, MarketplacePlugin, Roots, Source,
};
use std::cell::RefCell;

const MEASURE: f32 = 760.;
#[cfg(target_os = "windows")]
const PROSE_FONT: &str = "Segoe UI";
#[cfg(not(target_os = "windows"))]
const PROSE_FONT: &str = ".SystemUIFont";
/// The OpenAI plugins repository, offered as a one-click Codex marketplace.
const OPENAI_PLUGINS: &str = "https://github.com/openai/plugins";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Connected,
    Marketplace,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Only(Ecosystem),
}

struct State {
    tab: Tab,
    filter: Filter,
    category: Option<String>,
    loaded: bool,
    installed: Vec<InstalledPlugin>,
    market: Vec<MarketplacePlugin>,
    busy: Option<String>,
    message: Option<(bool, String)>,
}

thread_local! {
    // One plugins page at a time. Keeping its state here leaves `Panel` untouched.
    static STATE: RefCell<State> = RefCell::new(State {
        tab: Tab::Connected,
        filter: Filter::All,
        category: None,
        loaded: false,
        installed: Vec::new(),
        market: Vec::new(),
        busy: None,
        message: None,
    });
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|state| f(&mut state.borrow_mut()))
}

fn reload(state: &mut State) {
    if let Some(roots) = Roots::detect() {
        state.installed = plugins::installed(&roots);
        state.market = plugins::marketplace(&roots);
    }
    state.loaded = true;
}

fn pill(id: impl Into<gpui::ElementId>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .cursor_pointer()
        .rounded_full()
        .px_3()
        .py(px(3.))
        .text_size(px(12.))
}

fn chip(text: String) -> gpui::Div {
    let theme = Theme::global();
    div()
        .px(px(7.))
        .py(px(1.))
        .rounded_full()
        .bg(theme.INLINE_CODE_BG)
        .text_size(px(11.))
        .text_color(theme.TEXT_DIM)
        .child(text)
}

fn ecosystem_color(ecosystem: Ecosystem) -> gpui::Rgba {
    let theme = Theme::global();
    match ecosystem {
        Ecosystem::Claude => theme.USER_ACCENT,
        Ecosystem::Codex => theme.AI_ACCENT,
        Ecosystem::Jcode => theme.ACCENT,
    }
}

impl Panel {
    pub(crate) const PLUGINS_SESSION_ID: &str = "desktop://plugins";

    pub(crate) fn is_plugins(&self) -> bool {
        self.session_id == Self::PLUGINS_SESSION_ID
    }

    /// Run a plugin job off the UI thread, then refresh the lists.
    fn run_plugin_job(
        &mut self,
        label: String,
        job: impl FnOnce(&Roots) -> anyhow::Result<String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if with_state(|s| s.busy.is_some()) {
            return;
        }
        with_state(|s| {
            s.busy = Some(label);
            s.message = None;
        });
        cx.notify();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::Builder::new()
            .name("jcode-plugins".into())
            .spawn(move || {
                let result = match Roots::detect() {
                    Some(roots) => job(&roots).map_err(|e| format!("{e:#}")),
                    None => Err("Could not find the home folder.".into()),
                };
                let _ = tx.send_blocking(result);
            })
            .expect("spawn plugins worker");
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                let _ = this.update(cx, |_, cx| {
                    with_state(|s| {
                        s.busy = None;
                        s.message = Some(match result {
                            Ok(text) => (true, text),
                            Err(text) => (false, text),
                        });
                        reload(s);
                    });
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn install_plugin(
        &mut self,
        ecosystem: Ecosystem,
        marketplace: String,
        name: String,
        source: Source,
        mode: InstallMode,
        cx: &mut Context<Self>,
    ) {
        self.run_plugin_job(
            format!("Installing {name}…"),
            move |roots| {
                let report = plugins::install(roots, ecosystem, &marketplace, &name, &source, mode)?;
                let mut parts = vec![format!("Installed {name} into Jcode.")];
                if !report.skills.is_empty() {
                    parts.push(format!("Skills: {}.", report.skills.join(", ")));
                }
                if !report.mcp_servers.is_empty() {
                    parts.push(format!("MCP servers: {}.", report.mcp_servers.join(", ")));
                }
                if report.skills.is_empty() && report.mcp_servers.is_empty() {
                    parts.push("It had no skills or MCP servers Jcode can use.".into());
                }
                if !report.skipped.is_empty() {
                    parts.push(format!("Kept your existing {}.", report.skipped.join(", ")));
                }
                if !report.unsupported.is_empty() {
                    parts.push(format!("Not usable in Jcode: {}.", report.unsupported.join(", ")));
                }
                parts.push("Start a new session to use it.".into());
                Ok(parts.join(" "))
            },
            cx,
        );
    }

    fn uninstall_plugin(&mut self, key: String, cx: &mut Context<Self>) {
        self.run_plugin_job(
            format!("Removing {key}…"),
            move |roots| {
                plugins::uninstall(roots, &key)?;
                Ok(format!("Removed {key} from Jcode."))
            },
            cx,
        );
    }

    fn tab_button(&self, tab: Tab, label: String, cx: &Context<Self>) -> impl IntoElement {
        let theme = Theme::global();
        let selected = with_state(|s| s.tab == tab);
        pill(match tab {
            Tab::Connected => "plugins-tab-connected",
            Tab::Marketplace => "plugins-tab-marketplace",
        })
        .text_color(if selected { theme.TEXT } else { theme.TEXT_DIM })
        .when(selected, |el| el.bg(theme.HEADER_BG))
        .hover(|el| el.bg(theme.HEADER_BG).text_color(theme.TEXT))
        .on_click(cx.listener(move |_, _, _, cx| {
            with_state(|s| s.tab = tab);
            cx.notify();
        }))
        .child(label)
    }

    fn filter_button(
        &self,
        id: &'static str,
        label: &'static str,
        selected: bool,
        apply: impl Fn(&mut State) + 'static,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = Theme::global();
        pill(id)
            .text_color(if selected { theme.TEXT } else { theme.TEXT_DIM })
            .border_1()
            .border_color(if selected { theme.PANEL_BORDER_FOCUS } else { theme.PANEL_BORDER })
            .hover(|el| el.text_color(theme.TEXT))
            .on_click(cx.listener(move |_, _, _, cx| {
                with_state(|s| apply(s));
                cx.notify();
            }))
            .child(label)
    }

    fn action_button(
        &self,
        id: (&'static str, usize),
        label: &'static str,
        primary: bool,
        cx: &Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> impl IntoElement {
        let theme = Theme::global();
        let busy = with_state(|s| s.busy.is_some());
        pill(id)
            .flex_shrink_0()
            .when(primary, |el| el.bg(theme.ACCENT).text_color(theme.BG))
            .when(!primary, |el| {
                el.border_1().border_color(theme.PANEL_BORDER).text_color(theme.TEXT)
            })
            .when(busy, |el| el.opacity(0.5))
            .hover(|el| el.opacity(0.85))
            .on_click(cx.listener(move |panel, _, _, cx| {
                cx.stop_propagation();
                on_click(panel, cx);
            }))
            .child(label)
    }

    fn installed_row(&self, index: usize, plugin: &InstalledPlugin, cx: &Context<Self>) -> gpui::Div {
        let theme = Theme::global();
        let c = &plugin.contents;
        let mut chips: Vec<String> = Vec::new();
        if !c.skills.is_empty() {
            chips.push(format!("{} skill{}", c.skills.len(), if c.skills.len() == 1 { "" } else { "s" }));
        }
        if !c.mcp_servers.is_empty() {
            chips.push(format!("MCP: {}", c.mcp_servers.join(", ")));
        }
        if c.apps > 0 {
            chips.push(format!("{} app(s), Codex only", c.apps));
        }
        if c.commands > 0 {
            chips.push(format!("{} command(s)", c.commands));
        }
        if !plugin.enabled {
            chips.push("disabled".into());
        }
        let usable = !c.skills.is_empty() || !c.mcp_servers.is_empty();
        let key = plugin.key();
        let (ecosystem, marketplace, name, path) = (
            plugin.ecosystem,
            plugin.marketplace.clone(),
            plugin.name.clone(),
            plugin.path.clone(),
        );
        let button = if plugin.in_jcode {
            self.action_button(("plugin-remove", index), "Remove from Jcode", false, cx, move |panel, cx| {
                panel.uninstall_plugin(key.clone(), cx)
            })
            .into_any_element()
        } else if usable {
            self.action_button(("plugin-use", index), "Use in Jcode", true, cx, move |panel, cx| {
                panel.install_plugin(
                    ecosystem,
                    marketplace.clone(),
                    name.clone(),
                    Source::Local(path.clone()),
                    InstallMode::Full,
                    cx,
                )
            })
            .into_any_element()
        } else {
            div()
                .text_size(px(11.))
                .text_color(theme.TEXT_FAINT)
                .child("Nothing Jcode can use")
                .into_any_element()
        };
        row(
            plugin.ecosystem,
            plugin.title().to_owned(),
            plugin.marketplace.clone(),
            c.description.clone().unwrap_or_default(),
            chips,
            plugin.in_jcode,
            button,
        )
    }

    fn market_row(&self, index: usize, plugin: &MarketplacePlugin, cx: &Context<Self>) -> gpui::Div {
        let mut chips = Vec::new();
        if let Some(category) = &plugin.category {
            chips.push(category.clone());
        }
        if matches!(plugin.source, Source::Git { .. }) {
            chips.push("downloads from git".into());
        }
        let buttons = if plugin.in_jcode {
            let key = plugin.key();
            div()
                .flex()
                .gap_1()
                .child(self.action_button(("market-remove", index), "Remove", false, cx, move |panel, cx| {
                    panel.uninstall_plugin(key.clone(), cx)
                }))
        } else {
            let full = plugin.clone();
            let skills = plugin.clone();
            div()
                .flex()
                .gap_1()
                .child(self.action_button(("market-skills", index), "Skills only", false, cx, move |panel, cx| {
                    panel.install_plugin(
                        skills.ecosystem,
                        skills.marketplace.clone(),
                        skills.name.clone(),
                        skills.source.clone(),
                        InstallMode::SkillsOnly,
                        cx,
                    )
                }))
                .child(self.action_button(("market-install", index), "Install", true, cx, move |panel, cx| {
                    panel.install_plugin(
                        full.ecosystem,
                        full.marketplace.clone(),
                        full.name.clone(),
                        full.source.clone(),
                        InstallMode::Full,
                        cx,
                    )
                }))
        };
        row(
            plugin.ecosystem,
            plugin.name.clone(),
            plugin.marketplace.clone(),
            plugin.description.clone(),
            chips,
            plugin.in_jcode,
            buttons.into_any_element(),
        )
    }

    pub(super) fn render_plugins(&self, _window: &Window, cx: &Context<Self>) -> gpui::AnyElement {
        let theme = Theme::global();
        with_state(|s| {
            if !s.loaded {
                reload(s);
            }
        });
        let (tab, filter, category, busy, message, connected_count, market_count) = with_state(|s| {
            (
                s.tab,
                s.filter,
                s.category.clone(),
                s.busy.clone(),
                s.message.clone(),
                s.installed.len(),
                s.market.len(),
            )
        });
        let keep = |ecosystem: Ecosystem| match filter {
            Filter::All => true,
            Filter::Only(only) => only == ecosystem,
        };

        let mut list = div().flex().flex_col().gap_2();
        match tab {
            Tab::Connected => {
                let groups = with_state(|s| s.installed.clone());
                for ecosystem in [Ecosystem::Claude, Ecosystem::Codex, Ecosystem::Jcode] {
                    if !keep(ecosystem) {
                        continue;
                    }
                    let items: Vec<_> = groups
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| p.ecosystem == ecosystem)
                        .collect();
                    if items.is_empty() {
                        continue;
                    }
                    list = list.child(section_label(format!(
                        "{} · {}",
                        match ecosystem {
                            Ecosystem::Jcode => "Installed in Jcode",
                            other => other.label(),
                        },
                        items.len()
                    )));
                    for (index, plugin) in items {
                        list = list.child(self.installed_row(index, plugin, cx));
                    }
                }
                if connected_count == 0 {
                    list = list.child(empty("No Claude Code or Codex plugins found on this computer."));
                }
            }
            Tab::Marketplace => {
                let market = with_state(|s| s.market.clone());
                let mut categories: Vec<String> = market
                    .iter()
                    .filter(|p| keep(p.ecosystem))
                    .filter_map(|p| p.category.clone())
                    .collect();
                categories.sort();
                categories.dedup();
                let mut cats = div().flex().flex_wrap().gap_1().child(self.filter_button(
                    "plugins-cat-all",
                    "All categories",
                    category.is_none(),
                    |s| s.category = None,
                    cx,
                ));
                for (i, name) in categories.into_iter().enumerate() {
                    let selected = category.as_deref() == Some(name.as_str());
                    let value = name.clone();
                    cats = cats.child(
                        pill(("plugins-cat", i))
                            .text_color(if selected { theme.TEXT } else { theme.TEXT_DIM })
                            .border_1()
                            .border_color(if selected { theme.PANEL_BORDER_FOCUS } else { theme.PANEL_BORDER })
                            .hover(|el| el.text_color(theme.TEXT))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                let value = value.clone();
                                with_state(|s| s.category = Some(value));
                                cx.notify();
                            }))
                            .child(name),
                    );
                }
                list = list.child(cats);
                let shown: Vec<_> = market
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| keep(p.ecosystem))
                    .filter(|(_, p)| category.is_none() || p.category == category)
                    .collect();
                list = list.child(section_label(format!("{} plugins", shown.len())));
                for (index, plugin) in shown {
                    list = list.child(self.market_row(index, plugin, cx));
                }
                list = list.child(
                    div()
                        .mt_4()
                        .flex()
                        .items_center()
                        .gap_3()
                        .text_size(px(12.))
                        .text_color(theme.TEXT_DIM)
                        .child("More Codex plugins live in the OpenAI plugins repository.")
                        .child(self.action_button(("plugins-add-openai", 0), "Add OpenAI marketplace", false, cx, |panel, cx| {
                            panel.run_plugin_job(
                                "Downloading the OpenAI plugins marketplace…".into(),
                                |roots| {
                                    plugins::add_marketplace(roots, OPENAI_PLUGINS)?;
                                    Ok("Added the OpenAI plugins marketplace.".into())
                                },
                                cx,
                            )
                        })),
                );
            }
        }

        let filters = div()
            .flex()
            .gap_1()
            .child(self.filter_button("plugins-all", "All", filter == Filter::All, |s| {
                s.filter = Filter::All;
                s.category = None;
            }, cx))
            .child(self.filter_button(
                "plugins-claude",
                "Claude Code",
                filter == Filter::Only(Ecosystem::Claude),
                |s| {
                    s.filter = Filter::Only(Ecosystem::Claude);
                    s.category = None;
                },
                cx,
            ))
            .child(self.filter_button(
                "plugins-codex",
                "Codex",
                filter == Filter::Only(Ecosystem::Codex),
                |s| {
                    s.filter = Filter::Only(Ecosystem::Codex);
                    s.category = None;
                },
                cx,
            ));

        let status = busy
            .map(|text| (None, text))
            .or_else(|| message.map(|(ok, text)| (Some(ok), text)));

        div()
            .id("desktop-plugins")
            .debug_selector(|| "desktop-plugins".into())
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .font_family(PROSE_FONT)
            .text_size(px(14.))
            .text_color(theme.TEXT)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    window.dispatch_action(Box::new(crate::ui_actions::ClosePanel), cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div().px_5().py_3().flex_shrink_0().child(
                    div()
                        .w_full()
                        .max_w(px(MEASURE))
                        .mx_auto()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .mr_2()
                                        .text_size(px(16.))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("Plugins"),
                                )
                                .child(self.tab_button(Tab::Connected, format!("Connected · {connected_count}"), cx))
                                .child(self.tab_button(Tab::Marketplace, format!("Marketplace · {market_count}"), cx))
                                .child(div().flex_1())
                                .child(
                                    pill("plugins-refresh")
                                        .text_color(theme.TEXT_DIM)
                                        .hover(|el| el.bg(theme.HEADER_BG).text_color(theme.TEXT))
                                        .on_click(cx.listener(|_, _, _, cx| {
                                            with_state(reload);
                                            cx.notify();
                                        }))
                                        .child("Refresh"),
                                )
                                .child(
                                    pill("close-plugins")
                                        .text_color(theme.TEXT_DIM)
                                        .hover(|el| el.bg(theme.HEADER_BG).text_color(theme.TEXT))
                                        .on_click(|_, window, cx| {
                                            window.dispatch_action(
                                                Box::new(crate::ui_actions::ClosePanel),
                                                cx,
                                            )
                                        })
                                        .child("Close"),
                                ),
                        )
                        .child(filters)
                        .when_some(status, |el, (ok, text)| {
                            el.child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .text_size(px(12.))
                                    .bg(match ok {
                                        Some(false) => theme.ERROR_BG,
                                        _ => theme.HEADER_BG,
                                    })
                                    .text_color(match ok {
                                        Some(false) => theme.ERROR,
                                        Some(true) => theme.TEXT,
                                        None => theme.TEXT_DIM,
                                    })
                                    .child(text),
                            )
                        }),
                ),
            )
            .child(
                div()
                    .id(match tab {
                        Tab::Connected => "plugins-scroll-connected",
                        Tab::Marketplace => "plugins-scroll-marketplace",
                    })
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_5()
                    .pt_2()
                    .pb_10()
                    .child(div().w_full().max_w(px(MEASURE)).mx_auto().child(list)),
            )
            .into_any_element()
    }
}

fn section_label(text: String) -> gpui::Div {
    div()
        .mt_3()
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(Theme::global().TEXT_DIM)
        .child(text)
}

fn empty(text: &'static str) -> gpui::Div {
    div()
        .py_6()
        .text_size(px(13.))
        .text_color(Theme::global().TEXT_DIM)
        .child(text)
}

fn row(
    ecosystem: Ecosystem,
    title: String,
    marketplace: String,
    description: String,
    chips: Vec<String>,
    in_jcode: bool,
    action: gpui::AnyElement,
) -> gpui::Div {
    let theme = Theme::global();
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .py_2()
        .rounded_lg()
        .border_1()
        .border_color(theme.PANEL_BORDER)
        .bg(theme.PANEL_BG)
        .child(
            div()
                .flex_shrink_0()
                .size(px(8.))
                .rounded_full()
                .bg(ecosystem_color(ecosystem)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_size(px(13.))
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme.TEXT_FAINT)
                                .child(format!("{} · {marketplace}", ecosystem.label())),
                        )
                        .when(in_jcode, |el| {
                            el.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme.OK)
                                    .child("✓ in Jcode"),
                            )
                        }),
                )
                .when(!description.is_empty(), |el| {
                    el.child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(17.))
                            .text_color(theme.TEXT_DIM)
                            .line_clamp(2)
                            .text_ellipsis()
                            .child(description),
                    )
                })
                .when(!chips.is_empty(), |el| {
                    el.child(div().flex().flex_wrap().gap_1().children(chips.into_iter().map(chip)))
                }),
        )
        .child(action)
}
