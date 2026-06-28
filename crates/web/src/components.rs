//! Design System primitives — Leptos ports of the BagholderDeLorean JSX components.
//! Inline styles use CSS vars from ds.css. Hover/focus state uses reactive signals.
//! ponytail: `checked`/`value` props are static bools/strings (not signals). Wrap
//!   the component call in a reactive closure so the parent signal drives re-creation.

use leptos::*;

/// Item for BdTabs segmented control.
#[derive(Clone, Debug)]
pub struct TabItem {
    pub value: String,
    pub label: String,
}

// ─── BdButton ────────────────────────────────────────────────────────────────

#[component]
pub fn BdButton(
    #[prop(default = "primary".to_string())] variant: String,
    #[prop(default = "md".to_string())]      size: String,
    #[prop(default = false)]                 full_width: bool,
    #[prop(default = false)]                 disabled: bool,
    #[prop(optional)]                        on_click: Option<Box<dyn Fn() + 'static>>,
    children: Children,
) -> impl IntoView {
    let (hovered, set_hovered) = create_signal(false);
    let (pressed, set_pressed) = create_signal(false);

    let is_ghost   = variant == "ghost";
    let is_primary = variant == "primary";

    let (bg, fg, border) = match variant.as_str() {
        "secondary" => ("var(--surface-card)", "var(--text-strong)",    "var(--ink-800)"),
        "dark"      => ("var(--teal-700)",     "var(--text-on-ink)",    "var(--ink-900)"),
        "ghost"     => ("transparent",          "var(--text-strong)",   "transparent"),
        _           => ("var(--accent)",         "var(--text-on-accent)", "var(--ink-800)"),
    };
    let height = match size.as_str() {
        "sm" => "var(--control-sm)", "lg" => "var(--control-lg)", _ => "var(--control-md)",
    };
    let padding = match size.as_str() {
        "sm" => "0 14px", "lg" => "0 26px", _ => "0 18px",
    };
    let font_size = match size.as_str() {
        "sm" => "var(--text-sm)", "lg" => "var(--text-lg)", _ => "var(--text-base)",
    };
    let width        = if full_width { "100%" } else { "auto" };
    let cursor       = if disabled   { "not-allowed" } else { "pointer" };
    let opacity      = if disabled   { "0.55" }        else { "1" };
    let border_color = if is_ghost   { "transparent" }  else { border };

    let style = move || {
        let eff_bg = if hovered.get() && is_primary && !disabled { "var(--accent-hover)" }
            else if hovered.get() && is_ghost && !disabled        { "rgba(28,46,52,0.06)" }
            else                                                   { bg };
        let shadow = if is_ghost          { "none" }
            else if pressed.get()          { "var(--shadow-hard-sm)" }
            else if hovered.get()          { "var(--shadow-hard-lg)" }
            else                           { "var(--shadow-hard)" };
        let transform = if is_ghost        { "none" }
            else if pressed.get()          { "translate(2px,2px)" }
            else if hovered.get()          { "translate(-1px,-1px)" }
            else                           { "none" };
        format!(
            "display:inline-flex;align-items:center;justify-content:center;\
             gap:var(--space-2);height:{height};padding:{padding};width:{width};\
             font-family:var(--font-body);font-weight:var(--weight-bold);\
             font-size:{font_size};line-height:1;letter-spacing:0.005em;\
             color:{fg};background:{eff_bg};\
             border:var(--border-line) solid {border_color};\
             border-radius:var(--radius-md);box-shadow:{shadow};\
             transform:{transform};cursor:{cursor};opacity:{opacity};\
             transition:transform var(--dur-fast) var(--ease-out),\
               box-shadow var(--dur-fast) var(--ease-out),\
               background var(--dur-fast) var(--ease-out);\
             user-select:none;white-space:nowrap;"
        )
    };

    view! {
        <button
            type="button"
            disabled=disabled
            style=style
            on:mouseenter=move |_| { if !disabled { set_hovered.set(true); } }
            on:mouseleave=move |_| { set_hovered.set(false); set_pressed.set(false); }
            on:mousedown=move |_| { if !disabled && !is_ghost { set_pressed.set(true); } }
            on:mouseup=move |_| { set_pressed.set(false); }
            on:click=move |_| { if let Some(ref f) = on_click { f(); } }
        >
            {children()}
        </button>
    }
}

// ─── BdCard ──────────────────────────────────────────────────────────────────

#[component]
pub fn BdCard(
    #[prop(default = "paper".to_string())]          tone: String,
    #[prop(optional)]                               overline: Option<String>,
    #[prop(optional)]                               title: Option<String>,
    #[prop(default = "var(--space-5)".to_string())] padding: String,
    children: Children,
) -> impl IntoView {
    let dark = tone == "dark";
    let bg           = if dark { "var(--surface-ink)" }  else { "var(--surface-card)" };
    let color        = if dark { "var(--text-on-ink)" }  else { "var(--text-body)" };
    let border_color = if dark { "var(--ink-900)" }       else { "var(--ink-800)" };
    let ol_color     = if dark { "var(--text-on-ink-muted)" } else { "var(--text-muted)" };
    let title_color  = if dark { "var(--text-on-ink)" }  else { "var(--text-strong)" };

    let card_style = format!(
        "position:relative;background:{bg};color:{color};\
         border:var(--border-line) solid {border_color};\
         border-radius:var(--radius-lg);box-shadow:var(--shadow-hard);padding:{padding};"
    );
    view! {
        <div style=card_style>
            {overline.map(|o| view! {
                <span style=format!(
                    "font-weight:var(--weight-bold);font-size:var(--text-micro);\
                     letter-spacing:var(--tracking-overline);text-transform:uppercase;\
                     color:{ol_color};display:block;margin-bottom:4px;"
                )>{o}</span>
            })}
            {title.map(|t| view! {
                <h3 style=format!(
                    "font-family:var(--font-display);font-weight:var(--weight-bold);\
                     font-size:var(--text-title);line-height:var(--leading-snug);\
                     letter-spacing:var(--tracking-tight);color:{title_color};\
                     margin:0 0 var(--space-4) 0;"
                )>{t}</h3>
            })}
            {children()}
        </div>
    }
}

// ─── BdBadge ─────────────────────────────────────────────────────────────────

#[component]
pub fn BdBadge(
    #[prop(default = "neutral".to_string())] tone: String,
    #[prop(default = false)]                 soft: bool,
    children: Children,
) -> impl IntoView {
    let (solid_bg, soft_bg, soft_fg) = match tone.as_str() {
        "accent" => ("var(--accent)",   "var(--accent-soft)", "var(--rust-700)"),
        "gain"   => ("var(--gain)",     "var(--gain-200)",    "var(--gain-600)"),
        "loss"   => ("var(--loss)",     "var(--loss-200)",    "var(--loss-600)"),
        "warn"   => ("var(--warn)",     "#f0e0b8",            "#8a6a18"),
        _        => ("var(--ink-700)",  "var(--paper-200)",   "var(--ink-800)"),
    };
    let bg = if soft { soft_bg } else { solid_bg };
    let fg = if soft { soft_fg } else { "var(--paper-50)" };
    let style = format!(
        "display:inline-flex;align-items:center;gap:4px;\
         padding:3px 9px;font-family:var(--font-body);\
         font-weight:var(--weight-bold);font-size:var(--text-xs);\
         line-height:1.2;letter-spacing:0.01em;\
         border-radius:var(--radius-full);\
         border:var(--border-hair) solid var(--ink-800);\
         color:{fg};background:{bg};white-space:nowrap;"
    );
    view! { <span style=style>{children()}</span> }
}

// ─── BdStat ──────────────────────────────────────────────────────────────────

#[component]
pub fn BdStat(
    label: String,
    value: String,
    #[prop(optional)] delta: Option<String>,
    #[prop(optional)] delta_tone: Option<String>,
    #[prop(default = "md".to_string())] size: String,
    #[prop(default = false)]            on_dark: bool,
) -> impl IntoView {
    let value_size = match size.as_str() {
        "sm" => "var(--text-title)",
        "lg" => "var(--text-display-md)",
        _    => "var(--text-display-sm)",
    };
    let tone = delta_tone.unwrap_or_else(|| {
        delta.as_deref().map(|d| {
            let t = d.trim();
            if t.starts_with('+') || t.starts_with('↑')                       { "gain" }
            else if t.starts_with('-') || t.starts_with('−') || t.starts_with('↓') { "loss" }
            else                                                                { "neutral" }
        }).unwrap_or("neutral").to_string()
    });
    let delta_color = match tone.as_str() {
        "gain" => "var(--gain)",
        "loss" => "var(--loss)",
        "warn" => "var(--warn)",
        _      => if on_dark { "var(--text-on-ink-muted)" } else { "var(--text-muted)" },
    };
    let label_color = if on_dark { "var(--text-on-ink-muted)" } else { "var(--text-muted)" };
    let value_color = if on_dark { "var(--text-on-ink)" }       else { "var(--text-strong)" };

    view! {
        <div style="display:flex;flex-direction:column;gap:6px;">
            <span style=format!(
                "font-weight:var(--weight-bold);font-size:var(--text-micro);\
                 letter-spacing:var(--tracking-overline);text-transform:uppercase;\
                 color:{label_color};"
            )>{label}</span>
            <div style="display:flex;align-items:baseline;gap:var(--space-3);flex-wrap:wrap;">
                <span style=format!(
                    "font-family:var(--font-mono);font-variant-numeric:tabular-nums;\
                     font-weight:var(--weight-bold);font-size:{value_size};\
                     line-height:1;letter-spacing:-0.02em;color:{value_color};"
                )>{value}</span>
                {delta.map(|d| view! {
                    <span style=format!(
                        "font-family:var(--font-mono);font-variant-numeric:tabular-nums;\
                         font-weight:var(--weight-bold);font-size:var(--text-sm);\
                         color:{delta_color};"
                    )>{d}</span>
                })}
            </div>
        </div>
    }
}

// ─── BdCallout ───────────────────────────────────────────────────────────────

#[component]
pub fn BdCallout(
    #[prop(default = "neutral".to_string())] tone: String,
    #[prop(optional)]                        title: Option<String>,
    children: Children,
) -> impl IntoView {
    let (bg, _bar, fg) = match tone.as_str() {
        "accent" => ("var(--accent-soft)", "var(--accent)", "var(--rust-700)"),
        "gain"   => ("var(--gain-200)",    "var(--gain)",   "var(--gain-600)"),
        "loss"   => ("var(--loss-200)",    "var(--loss)",   "var(--loss-600)"),
        "warn"   => ("#f3e6c2",            "var(--warn)",   "#8a6a18"),
        _        => ("var(--paper-200)",   "var(--ink-700)", "var(--text-body)"),
    };
    let style = format!(
        "display:flex;gap:var(--space-3);padding:var(--space-4);\
         background:{bg};border:var(--border-line) solid var(--ink-800);\
         border-left-width:var(--border-bold);\
         border-radius:var(--radius-md);color:var(--text-body);"
    );
    view! {
        <div role="note" style=style>
            <div style="display:flex;flex-direction:column;gap:2px;min-width:0;">
                {title.map(|t| view! {
                    <span style=format!(
                        "font-weight:var(--weight-bold);font-size:var(--text-sm);color:{fg};"
                    )>{t}</span>
                })}
                <span style="font-size:var(--text-sm);line-height:var(--leading-normal);">
                    {children()}
                </span>
            </div>
        </div>
    }
}

// ─── BdInput ─────────────────────────────────────────────────────────────────

#[component]
pub fn BdInput(
    #[prop(optional)]                label: Option<String>,
    #[prop(optional)]                hint: Option<String>,
    #[prop(optional)]                error: Option<String>,
    #[prop(default = "md".to_string())] size: String,
    #[prop(default = false)]         mono: bool,
    #[prop(default = String::new())] value: String,
    #[prop(optional)]                placeholder: Option<String>,
    #[prop(optional)]                list: Option<String>,
    #[prop(optional)]                on_input: Option<Box<dyn Fn(String) + 'static>>,
) -> impl IntoView {
    let (focused, set_focused) = create_signal(false);

    let height      = match size.as_str() { "sm" => "var(--control-sm)", "lg" => "var(--control-lg)", _ => "var(--control-md)" };
    let font_size   = if size == "sm" { "var(--text-sm)" } else { "var(--text-base)" };
    let font_family = if mono { "var(--font-mono)" } else { "var(--font-body)" };
    let has_error   = error.is_some();
    let border_c    = if has_error { "var(--loss)" } else { "var(--ink-800)" };

    let wrap_style = move || format!(
        "display:flex;align-items:center;gap:var(--space-2);\
         height:{height};padding:0 12px;background:var(--surface-card);\
         border:var(--border-line) solid {border_c};\
         border-radius:var(--radius-md);\
         box-shadow:{};transition:box-shadow var(--dur-fast) var(--ease-out);",
        if focused.get() { "0 0 0 3px var(--ring),var(--shadow-inset)" }
        else             { "var(--shadow-inset)" }
    );
    let input_style = format!(
        "flex:1;min-width:0;border:none;outline:none;background:transparent;\
         font-family:{font_family};font-size:{font_size};color:var(--text-body);"
    );
    let msg = error.clone().or(hint.clone());

    view! {
        <label style="display:flex;flex-direction:column;gap:var(--space-2);">
            {label.map(|l| view! {
                <span style="font-size:var(--text-sm);font-weight:var(--weight-semibold);color:var(--text-strong);">
                    {l}
                </span>
            })}
            <span style=wrap_style>
                <input
                    prop:value=value
                    placeholder=placeholder.unwrap_or_default()
                    list=list.unwrap_or_default()
                    style=input_style
                    on:focus=move |_| set_focused.set(true)
                    on:blur=move |_| set_focused.set(false)
                    on:input=move |e| { if let Some(ref f) = on_input { f(event_target_value(&e)); } }
                />
            </span>
            {msg.map(|m| view! {
                <span style=format!(
                    "font-size:var(--text-xs);color:{};",
                    if has_error { "var(--loss)" } else { "var(--text-muted)" }
                )>{m}</span>
            })}
        </label>
    }
}

// ─── BdSelect ────────────────────────────────────────────────────────────────

#[component]
pub fn BdSelect(
    #[prop(optional)]                   label: Option<String>,
    #[prop(optional)]                   hint: Option<String>,
    #[prop(default = "md".to_string())] size: String,
    #[prop(optional)]                   on_change: Option<Box<dyn Fn(String) + 'static>>,
    children: Children,
) -> impl IntoView {
    let (focused, set_focused) = create_signal(false);

    let height    = match size.as_str() { "sm" => "var(--control-sm)", "lg" => "var(--control-lg)", _ => "var(--control-md)" };
    let font_size = if size == "sm" { "var(--text-sm)" } else { "var(--text-base)" };

    let wrap_style = move || format!(
        "position:relative;display:block;height:{height};\
         border:var(--border-line) solid var(--ink-800);\
         border-radius:var(--radius-md);background:var(--surface-card);\
         box-shadow:{};transition:box-shadow var(--dur-fast);",
        if focused.get() { "0 0 0 3px var(--ring),var(--shadow-inset)" }
        else             { "var(--shadow-inset)" }
    );
    let select_style = format!(
        "appearance:none;-webkit-appearance:none;width:100%;height:100%;\
         padding:0 36px 0 12px;border:none;outline:none;background:transparent;\
         font-family:var(--font-body);font-size:{font_size};\
         font-weight:var(--weight-medium);color:var(--text-body);cursor:pointer;"
    );

    view! {
        <label style="display:flex;flex-direction:column;gap:var(--space-2);">
            {label.map(|l| view! {
                <span style="font-size:var(--text-sm);font-weight:var(--weight-semibold);color:var(--text-strong);">
                    {l}
                </span>
            })}
            <span style=wrap_style>
                <select
                    style=select_style
                    on:focus=move |_| set_focused.set(true)
                    on:blur=move |_| set_focused.set(false)
                    on:change=move |e| { if let Some(ref f) = on_change { f(event_target_value(&e)); } }
                >
                    {children()}
                </select>
                <span aria-hidden="true" style="\
                    position:absolute;right:12px;top:50%;transform:translateY(-50%);\
                    width:9px;height:9px;\
                    border-right:2px solid var(--ink-800);\
                    border-bottom:2px solid var(--ink-800);\
                    rotate:45deg;pointer-events:none;margin-top:-3px;" />
            </span>
            {hint.map(|h| view! {
                <span style="font-size:var(--text-xs);color:var(--text-muted);">{h}</span>
            })}
        </label>
    }
}

// ─── BdTabs ──────────────────────────────────────────────────────────────────

#[component]
pub fn BdTabs(
    items: Vec<TabItem>,
    value: String,
    #[prop(default = false)] full_width: bool,
    #[prop(optional)]        on_change: Option<Box<dyn Fn(String) + 'static>>,
) -> impl IntoView {
    let container_style = format!(
        "display:{};width:{};padding:5px;gap:5px;\
         background:var(--surface-sunken);\
         border:var(--border-line) solid var(--ink-800);\
         border-radius:var(--radius-md);box-shadow:var(--shadow-inset);box-sizing:border-box;",
        if full_width { "flex" } else { "inline-flex" },
        if full_width { "100%" } else { "auto" },
    );

    let on_change = std::rc::Rc::new(on_change);
    let active    = value;

    let tabs = items.into_iter().map(move |item| {
        let is_active  = item.value == active;
        let cb         = on_change.clone();
        let click_val  = item.value.clone();

        let tab_style = format!(
            "display:inline-flex;align-items:center;justify-content:center;gap:6px;\
             flex:{};padding:7px 14px;\
             font-family:var(--font-body);font-weight:var(--weight-bold);\
             font-size:var(--text-sm);\
             color:{};background:{};\
             border:{};border-radius:var(--radius-sm);\
             box-shadow:{};cursor:pointer;\
             transition:background var(--dur-fast),color var(--dur-fast);\
             white-space:nowrap;",
            if full_width { "1 1 0" } else { "none" },
            if is_active { "var(--text-on-accent)" } else { "var(--text-muted)" },
            if is_active { "var(--accent)" }          else { "transparent" },
            if is_active { "var(--border-hair) solid var(--ink-800)" }
                else     { "var(--border-hair) solid transparent" },
            if is_active { "var(--shadow-hard-sm)" }  else { "none" },
        );

        view! {
            <button
                type="button"
                role="tab"
                aria-selected=is_active.to_string()
                style=tab_style
                on:click=move |_| {
                    if let Some(ref f) = *cb { f(click_val.clone()); }
                }
            >
                {item.label}
            </button>
        }
    }).collect_view();

    view! { <div role="tablist" style=container_style>{tabs}</div> }
}

// ─── BdSwitch ────────────────────────────────────────────────────────────────

#[component]
pub fn BdSwitch(
    #[prop(default = false)] checked: bool,
    #[prop(optional)]        label: Option<String>,
    #[prop(default = false)] disabled: bool,
    #[prop(optional)]        on_change: Option<Box<dyn Fn(bool) + 'static>>,
) -> impl IntoView {
    let track_style = format!(
        "position:relative;width:46px;height:26px;flex:none;\
         background:{};border:var(--border-line) solid var(--ink-800);\
         border-radius:var(--radius-full);\
         cursor:{};opacity:{};\
         transition:background var(--dur) var(--ease-out);\
         box-shadow:var(--shadow-inset);",
        if checked  { "var(--accent)" } else { "var(--paper-300)" },
        if disabled { "not-allowed" }   else { "pointer" },
        if disabled { "0.55" }          else { "1" },
    );
    let knob_style = format!(
        "position:absolute;top:1px;left:{}px;\
         width:20px;height:20px;\
         background:var(--paper-50);border:var(--border-line) solid var(--ink-800);\
         border-radius:50%;\
         transition:left var(--dur) var(--ease-lurch);",
        if checked { 21 } else { 1 },
    );

    view! {
        <label style=format!(
            "display:inline-flex;align-items:center;gap:var(--space-3);cursor:{};",
            if disabled { "not-allowed" } else { "pointer" }
        )>
            <span style=track_style role="presentation">
                <input
                    type="checkbox"
                    prop:checked=checked
                    disabled=disabled
                    on:change=move |e| {
                        if let Some(ref f) = on_change { f(event_target_checked(&e)); }
                    }
                    style="position:absolute;opacity:0;width:100%;height:100%;margin:0;cursor:inherit;"
                />
                <span style=knob_style />
            </span>
            {label.map(|l| view! {
                <span style="font-size:var(--text-sm);font-weight:var(--weight-semibold);color:var(--text-strong);">
                    {l}
                </span>
            })}
        </label>
    }
}

// ─── BdTag ───────────────────────────────────────────────────────────────────

#[component]
pub fn BdTag(
    #[prop(default = false)] selected: bool,
    #[prop(optional)]        on_remove: Option<Box<dyn Fn() + 'static>>,
    #[prop(optional)]        on_click: Option<Box<dyn Fn() + 'static>>,
    children: Children,
) -> impl IntoView {
    let cursor     = if on_click.is_some() { "pointer" } else { "default" };
    let has_remove = on_remove.is_some();
    let on_remove  = on_remove.map(std::rc::Rc::new);

    let style = format!(
        "display:inline-flex;align-items:center;gap:6px;padding:4px 10px;\
         font-family:var(--font-mono);font-size:var(--text-xs);\
         font-weight:var(--weight-bold);letter-spacing:0.01em;\
         color:{};background:{};\
         border:var(--border-line) solid var(--ink-800);\
         border-radius:var(--radius-full);cursor:{cursor};\
         transition:background var(--dur-fast),color var(--dur-fast);",
        if selected { "var(--text-on-accent)" } else { "var(--text-strong)" },
        if selected { "var(--accent)" }          else { "var(--surface-card)" },
    );

    let remove_btn = has_remove.then(|| {
        let cb = on_remove;
        view! {
            <button
                type="button"
                aria-label="Remove"
                on:click=move |e: web_sys::MouseEvent| {
                    e.stop_propagation();
                    if let Some(ref f) = cb { f(); }
                }
                style="display:inline-flex;align-items:center;justify-content:center;\
                       width:14px;height:14px;padding:0;border:none;border-radius:50%;\
                       background:transparent;color:inherit;cursor:pointer;\
                       font-size:13px;line-height:1;opacity:0.7;"
            >"×"</button>
        }
    });

    view! {
        <span
            style=style
            on:click=move |_| { if let Some(ref f) = on_click { f(); } }
        >
            {children()}
            {remove_btn}
        </span>
    }
}
