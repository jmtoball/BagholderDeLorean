//! Design System primitives — Leptos ports of the BagholderDeLorean JSX components.
//! Inline styles use CSS vars from ds.css. Hover/focus state uses reactive signals.
//! ponytail: `checked`/`value` props are static bools/strings (not signals). Wrap
//!   the component call in a reactive closure so the parent signal drives re-creation.
//! ponytail: px are tokenized only where a token matches exactly; remaining raw px
//!   are off-grid fine-tuning or control geometry with no token equivalent.

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
    #[prop(default = "var(--space-5)".to_string())] padding:String,
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
                     color:{ol_color};display:block;margin-bottom:var(--space-1);"
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
        "warn"   => ("var(--warn)",     "var(--warn-200)",    "var(--warn-600)"),
        _        => ("var(--ink-700)",  "var(--paper-200)",   "var(--ink-800)"),
    };
    let bg = if soft { soft_bg } else { solid_bg };
    let fg = if soft { soft_fg } else { "var(--paper-50)" };
    let style = format!(
        "display:inline-flex;align-items:center;gap:var(--space-1);\
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
    /// Tint the value itself gain/loss/warn (good/bad KPI coloring). Omit to keep
    /// the default strong ink. Mirrors the DS `Stat`'s `valueTone`.
    #[prop(optional)] value_tone: Option<String>,
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
    let value_color = match value_tone.as_deref() {
        Some("gain") => "var(--gain)",
        Some("loss") => "var(--loss)",
        Some("warn") => "var(--warn)",
        _ => if on_dark { "var(--text-on-ink)" } else { "var(--text-strong)" },
    };

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
    /// Optional leading icon (registry name), tinted with the tone's bar color.
    #[prop(optional)]                        icon: Option<String>,
    children: Children,
) -> impl IntoView {
    let (bg, bar, fg) = match tone.as_str() {
        "accent" => ("var(--accent-soft)", "var(--accent)", "var(--rust-700)"),
        "gain"   => ("var(--gain-200)",    "var(--gain)",   "var(--gain-600)"),
        "loss"   => ("var(--loss-200)",    "var(--loss)",   "var(--loss-600)"),
        "warn"   => ("var(--warn-200)",    "var(--warn)",   "var(--warn-600)"),
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
            {icon.map(|name| view! {
                <span style=format!("color:{bar};flex:none;margin-top:1px;")>
                    <Icon name=name size=20 />
                </span>
            })}
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
    /// Adornment text inside the field, before/after the input (e.g. "$", "%").
    #[prop(optional)]                prefix: Option<String>,
    #[prop(optional)]                suffix: Option<String>,
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
         height:{height};padding:0 var(--space-3);background:var(--surface-card);\
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
    let adorn_style = format!(
        "flex:none;font-family:{font_family};font-size:{font_size};\
         color:var(--text-muted);user-select:none;"
    );
    let prefix_el = prefix.map(|p| view! { <span style=adorn_style.clone()>{p}</span> });
    let suffix_el = suffix.map(|s| view! { <span style=adorn_style.clone()>{s}</span> });

    view! {
        <label style="display:flex;flex-direction:column;gap:var(--space-2);">
            {label.map(|l| view! {
                <span style="font-size:var(--text-sm);font-weight:var(--weight-semibold);color:var(--text-strong);">
                    {l}
                </span>
            })}
            <span style=wrap_style>
                {prefix_el}
                <input
                    prop:value=value
                    placeholder=placeholder.unwrap_or_default()
                    list=list.unwrap_or_default()
                    style=input_style
                    on:focus=move |_| set_focused.set(true)
                    on:blur=move |_| set_focused.set(false)
                    on:input=move |e| { if let Some(ref f) = on_input { f(event_target_value(&e)); } }
                />
                {suffix_el}
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
    /// Controlled value: forces the shown option to match, even when a reactive
    /// parent re-creates the select. `None` = uncontrolled (native default).
    #[prop(optional)]                   value: Option<String>,
    #[prop(optional)]                   on_change: Option<Box<dyn Fn(String) + 'static>>,
    children: Children,
) -> impl IntoView {
    let (focused, set_focused) = create_signal(false);

    // Controlled value: a native <select> shows its first option when re-created
    // with no selection, so set `.value` in an effect once the element (and its
    // options) are mounted. `prop:value` runs too early — before the options exist.
    let select_ref = create_node_ref::<html::Select>();
    let controlled = value.clone();
    create_effect(move |_| {
        if let (Some(el), Some(v)) = (select_ref.get(), controlled.as_ref()) {
            el.set_value(v);
        }
    });

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
         padding:0 36px 0 var(--space-3);border:none;outline:none;background:transparent;\
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
                    node_ref=select_ref
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
                    border-right:var(--border-line) solid var(--ink-800);\
                    border-bottom:var(--border-line) solid var(--ink-800);\
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

// ─── BdCheckbox ──────────────────────────────────────────────────────────────
// Brand selection box for table rows (ink border, accent fill + check when on).
// Like BdSwitch, `checked` is static — wrap the call in a reactive closure so a
// parent signal drives the visual state.

#[component]
pub fn BdCheckbox(
    #[prop(default = false)] checked: bool,
    #[prop(default = false)] disabled: bool,
    #[prop(optional)]        on_change: Option<Box<dyn Fn(bool) + 'static>>,
) -> impl IntoView {
    let box_style = format!(
        "position:relative;display:inline-flex;align-items:center;justify-content:center;\
         width:20px;height:20px;flex:none;color:var(--paper-50);\
         background:{};border:var(--border-line) solid var(--ink-800);\
         border-radius:var(--radius-sm);box-shadow:var(--shadow-inset);\
         transition:background var(--dur-fast) var(--ease-out);",
        if checked { "var(--accent)" } else { "var(--surface-card)" },
    );
    view! {
        <label style=format!(
            "display:inline-flex;align-items:center;cursor:{};",
            if disabled { "not-allowed" } else { "pointer" }
        )>
            <span style=box_style>
                <input
                    type="checkbox"
                    prop:checked=checked
                    disabled=disabled
                    on:change=move |e| { if let Some(ref f) = on_change { f(event_target_checked(&e)); } }
                    style="position:absolute;inset:0;opacity:0;margin:0;cursor:inherit;"
                />
                {checked.then(|| view! {
                    <span style="font-family:var(--font-body);font-weight:var(--weight-bold);\
                                 font-size:13px;line-height:1;">"\u{2713}"</span>
                })}
            </span>
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
        "display:inline-flex;align-items:center;gap:6px;padding:var(--space-1) 10px;\
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

// ─── Icon ─────────────────────────────────────────────────────────────────────

/// Inline-SVG icon (Lucide geometry). The brand's documented icon set is Lucide;
/// the app is WASM so we inline the paths rather than pull a font/CDN. Color via
/// `currentColor`. Add a `name` arm as new icons are needed.
#[component]
pub fn Icon(name: String, #[prop(default = 16)] size: usize) -> impl IntoView {
    let inner = match name.as_str() {
        "receipt" => "<path d=\"M4 2v20l2-1 2 1 2-1 2 1 2-1 2 1 2-1 2 1V2l-2 1-2-1-2 1-2-1-2 1-2-1-2 1Z\"/><path d=\"M16 8h-6\"/><path d=\"M16 12h-6\"/><path d=\"M16 16h-6\"/>",
        "info" => "<circle cx=\"12\" cy=\"12\" r=\"10\"/><path d=\"M12 16v-4\"/><path d=\"M12 8h.01\"/>",
        "plus" => "<path d=\"M5 12h14\"/><path d=\"M12 5v14\"/>",
        "x" => "<path d=\"M18 6 6 18\"/><path d=\"m6 6 12 12\"/>",
        "chevron-down" => "<path d=\"m6 9 6 6 6-6\"/>",
        "chevron-up" => "<path d=\"m18 15-6-6-6 6\"/>",
        "minus-circle" => "<circle cx=\"12\" cy=\"12\" r=\"10\"/><path d=\"M8 12h8\"/>",
        "rewind" => "<polygon points=\"11 19 2 12 11 5 11 19\"/><polygon points=\"22 19 13 12 22 5 22 19\"/>",
        "sliders-horizontal" => "<line x1=\"21\" x2=\"14\" y1=\"4\" y2=\"4\"/><line x1=\"10\" x2=\"3\" y1=\"4\" y2=\"4\"/><line x1=\"21\" x2=\"12\" y1=\"12\" y2=\"12\"/><line x1=\"8\" x2=\"3\" y1=\"12\" y2=\"12\"/><line x1=\"21\" x2=\"16\" y1=\"20\" y2=\"20\"/><line x1=\"12\" x2=\"3\" y1=\"20\" y2=\"20\"/><line x1=\"14\" x2=\"14\" y1=\"2\" y2=\"6\"/><line x1=\"8\" x2=\"8\" y1=\"10\" y2=\"14\"/><line x1=\"16\" x2=\"16\" y1=\"18\" y2=\"22\"/>",
        "bar-chart-3" => "<path d=\"M3 3v18h18\"/><path d=\"M18 17V9\"/><path d=\"M13 17V5\"/><path d=\"M8 17v-3\"/>",
        "alert-triangle" => "<path d=\"m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3Z\"/><path d=\"M12 9v4\"/><path d=\"M12 17h.01\"/>",
        "bookmark" => "<path d=\"m19 21-7-4-7 4V5a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v16z\"/>",
        "check" => "<path d=\"M20 6 9 17l-5-5\"/>",
        "settings-2" => "<path d=\"M20 7h-9\"/><path d=\"M14 17H5\"/><circle cx=\"17\" cy=\"17\" r=\"3\"/><circle cx=\"7\" cy=\"7\" r=\"3\"/>",
        "layout-grid" => "<rect width=\"7\" height=\"7\" x=\"3\" y=\"3\" rx=\"1\"/><rect width=\"7\" height=\"7\" x=\"14\" y=\"3\" rx=\"1\"/><rect width=\"7\" height=\"7\" x=\"14\" y=\"14\" rx=\"1\"/><rect width=\"7\" height=\"7\" x=\"3\" y=\"14\" rx=\"1\"/>",
        "trending-up" => "<polyline points=\"22 7 13.5 15.5 8.5 10.5 2 17\"/><polyline points=\"16 7 22 7 22 13\"/>",
        _ => "",
    };
    let s = size.to_string();
    view! {
        <svg width=s.clone() height=s viewBox="0 0 24 24" fill="none"
             stroke="currentColor" stroke-width="2" stroke-linecap="round"
             stroke-linejoin="round" inner_html=inner
             style="display:block;flex:none;" />
    }
}

// ─── SectionNav ────────────────────────────────────────────────────────────────

/// A fixed vertical rail of pills for navigating a stack of full-screen sections.
/// The active pill fills with the accent; clicking jumps. Layout-only — the
/// consumer owns scroll + active-index tracking. Ports `SectionNav` from
/// `design_system/components/core`.
#[component]
pub fn BdSectionNav(
    items: Vec<String>,
    #[prop(into)] active: Signal<usize>,
    #[prop(into)] on_jump: Callback<usize>,
) -> impl IntoView {
    view! {
        <nav aria-label="Sections"
             style="position:fixed;top:50%;right:18px;transform:translateY(-50%);z-index:60;\
                    display:flex;flex-direction:column;gap:9px;">
            {items.into_iter().enumerate().map(|(i, label)| {
                let pill = move || format!(
                    "display:flex;align-items:center;gap:7px;cursor:pointer;padding:5px 11px;\
                     background:{};color:{};border:var(--border-line) solid var(--ink-900);\
                     border-radius:var(--radius-full);box-shadow:{};\
                     font-family:var(--font-mono);font-weight:700;font-size:10px;\
                     letter-spacing:0.08em;text-transform:uppercase;\
                     transition:background var(--dur) var(--ease-out),color var(--dur) var(--ease-out);",
                    if active.get() == i { "var(--accent)" } else { "var(--surface-card-glass)" },
                    if active.get() == i { "var(--paper-50)" } else { "var(--ink-800)" },
                    if active.get() == i { "var(--shadow-hard-sm)" } else { "none" },
                );
                let dot = move || format!(
                    "width:7px;height:7px;border-radius:50%;background:{};",
                    if active.get() == i { "var(--paper-50)" } else { "var(--ink-500)" },
                );
                view! {
                    <button type="button" title=label.clone()
                        aria-current=move || active.get().eq(&i).then_some("true")
                        on:click=move |_| on_jump.call(i)
                        style=pill>
                        <span style=dot />
                        {label}
                    </button>
                }
            }).collect_view()}
        </nav>
    }
}

// ─── SiteFooter ────────────────────────────────────────────────────────────────

/// One nav link in a [`BdSiteFooter`].
#[derive(Clone, Debug)]
pub struct FooterLink {
    pub label: String,
    pub href: String,
}

/// A full-bleed image footer: the supplied artwork covers the band, a bottom
/// scrim keeps text legible, a mono uppercase tagline sits up top and the nav
/// links anchor the bottom-right. Ports `SiteFooter` from
/// `design_system/components/core`.
#[component]
pub fn BdSiteFooter(
    #[prop(into)] image: String,
    #[prop(into)] tagline: String,
    links: Vec<FooterLink>,
    #[prop(default = 320)] height: usize,
) -> impl IntoView {
    let shell = format!(
        "position:relative;overflow:hidden;min-height:{height}px;\
         border-top:var(--border-bold) solid var(--ink-900);\
         background-color:var(--ink-900);background-image:url({image});\
         background-size:cover;background-position:center bottom;"
    );
    let inner = format!(
        "position:relative;box-sizing:border-box;max-width:1320px;min-height:{height}px;\
         margin:0 auto;padding:28px 56px;display:flex;flex-direction:column;\
         justify-content:space-between;"
    );
    view! {
        <footer style=shell>
            // legibility scrim along the bottom edge
            <div aria-hidden="true" style="position:absolute;inset:0;pointer-events:none;\
                 background:linear-gradient(to bottom,rgba(15,18,26,0) 55%,rgba(15,18,26,0.55) 100%);" />
            <div style=inner>
                <span style="font-family:var(--font-mono);font-size:12px;letter-spacing:0.16em;\
                             text-transform:uppercase;color:rgba(255,255,255,0.55);\
                             text-shadow:0 1px 6px rgba(0,0,0,0.6);">
                    {tagline}
                </span>
                <div style="display:flex;align-items:flex-end;justify-content:flex-end;\
                            gap:24px;flex-wrap:wrap;">
                    <nav style="display:flex;align-items:center;gap:28px;font-weight:600;font-size:14px;">
                        {links.into_iter().map(|l| view! {
                            <a href=l.href style="color:rgba(255,255,255,0.82);text-decoration:none;\
                                text-shadow:0 1px 6px rgba(0,0,0,0.6);">{l.label}</a>
                        }).collect_view()}
                    </nav>
                </div>
            </div>
        </footer>
    }
}

// ─── Overline ──────────────────────────────────────────────────────────────────

/// The small monospace kicker that sits above a section or card title. Three
/// tones so it reads on paper (`accent`, default), muted, or over dark teal
/// panels (`on-ink`). Ports `Overline` from `design_system/components/core`.
#[component]
pub fn Overline(
    #[prop(into, optional)] tone: Option<String>,
    #[prop(into, optional)] style: Option<String>,
    children: Children,
) -> impl IntoView {
    let color = match tone.as_deref() {
        Some("muted") => "var(--text-muted)",
        Some("on-ink") => "var(--text-on-ink-muted)",
        _ => "var(--accent)",
    };
    let style = format!(
        "font-family:var(--font-mono);font-weight:700;font-size:var(--text-micro);\
         letter-spacing:var(--tracking-overline);text-transform:uppercase;color:{};{}",
        color,
        style.unwrap_or_default(),
    );
    view! { <div style=style>{children()}</div> }
}

// ─── YearStepper ───────────────────────────────────────────────────────────────

/// A numeric −5/−1 · field · +1/+5 stepper for a year, clamped to `[min, max]`
/// with buttons disabling at the bounds. `tone="accent"` rings it in accent (used
/// for the To-year while projecting). Ports the DS `Stepper` (step 1, bigStep 5).
/// `value` is static — wrap the call in a reactive closure so a signal drives it.
#[component]
pub fn BdYearStepper(
    value: u32,
    min: u32,
    max: u32,
    #[prop(default = "ink".to_string())] tone: String,
    #[prop(into)] on_change: Callback<u32>,
) -> impl IntoView {
    let ring = if tone == "accent" { "var(--accent)" } else { "var(--ink-800)" };
    let clamp = move |v: i64| v.max(min as i64).min(max as i64) as u32;
    let btn = move |delta: i64, label: &'static str| {
        let target = clamp(value as i64 + delta);
        let dis = target == value;
        let style = format!(
            "min-width:38px;height:var(--control-md);padding:0 8px;font-family:var(--font-mono);\
             font-weight:700;font-size:12.5px;color:{};background:var(--surface-card);\
             border:2px solid {ring};border-radius:var(--radius-sm);cursor:{};opacity:{};",
            if dis { "var(--text-faint)" } else { "var(--text-strong)" },
            if dis { "not-allowed" } else { "pointer" },
            if dis { "0.45" } else { "1" },
        );
        view! {
            <button type="button" disabled=dis aria-label=format!("{} by {}", if delta > 0 { "Increase" } else { "Decrease" }, delta.abs())
                on:click=move |_| on_change.call(target) style=style>{label}</button>
        }
    };
    let input_style = format!(
        "width:78px;height:var(--control-md);text-align:center;font-family:var(--font-mono);\
         font-weight:700;font-size:15px;color:var(--text-strong);background:var(--surface-sunken);\
         border:2px solid {ring};border-radius:var(--radius-sm);box-sizing:border-box;"
    );
    view! {
        <div style="display:flex;align-items:stretch;gap:6px;">
            {btn(-5, "\u{2212}5")}
            {btn(-1, "\u{2212}1")}
            <input type="number" prop:value=value.to_string()
                min=min.to_string() max=max.to_string()
                on:change=move |ev| {
                    if let Ok(v) = event_target_value(&ev).parse::<i64>() { on_change.call(clamp(v)); }
                }
                style=input_style />
            {btn(1, "+1")}
            {btn(5, "+5")}
        </div>
    }
}

// ─── RateChips ─────────────────────────────────────────────────────────────────

/// One chip in a [`RateChips`] row.
#[derive(Clone, Debug)]
pub struct Chip {
    pub label: String,
    /// Lit (active) — the bracket/threshold the current inputs land in.
    pub on: bool,
}

/// A mono chip row that lights the active option — used to show which tax bracket
/// an income lands in. Mirrors `RateChips` in `ui_kits/webapp/TaxSim.jsx`.
#[component]
pub fn RateChips(chips: Vec<Chip>) -> impl IntoView {
    view! {
        <div style="display:flex;gap:6px;flex-wrap:wrap;">
            {chips.into_iter().map(|c| {
                let on = c.on;
                let style = format!(
                    "font-family:var(--font-mono);font-weight:700;font-size:12px;\
                     padding:5px 10px;border-radius:999px;border:2px solid {};\
                     background:{};color:{};box-shadow:{};",
                    if on { "var(--ink-900)" } else { "var(--paper-300)" },
                    if on { "var(--accent-soft)" } else { "transparent" },
                    if on { "var(--ink-900)" } else { "var(--text-faint)" },
                    if on { "var(--shadow-hard-sm)" } else { "none" },
                );
                view! { <span style=style>{c.label}</span> }
            }).collect_view()}
        </div>
    }
}
