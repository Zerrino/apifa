//! The authentication editor: the workspace's (Settings), a folder's, a request's.

use egui::{ComboBox, RichText, TextEdit, TextStyle, Ui};

use crate::auth::{Auth, ClientAuth, Grant, KeyLocation, OAuth2};
use crate::theme;
use crate::widgets::*;

/// One labelled text field; returns whether it changed.
fn row(ui: &mut Ui, label: &str, value: &mut String, hint: &str, secret: bool, show: bool) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        fixed(ui, 130.0, |ui| {
            ui.label(RichText::new(label).color(theme::MUTED));
        });
        changed = ui
            .add(TextEdit::singleline(value).font(TextStyle::Monospace).password(secret && !show).hint_text(hint_rt(hint)).desired_width(ui.available_width().min(460.0)))
            .changed();
    });
    changed
}

/// Edits `auth`. `inherit_from`: what `Inherit` means here (None: Inherit isn't offered).
/// `show_secrets` is the eye toggle, kept by the caller. Returns whether anything changed; sets
/// `*get_token` when the user asks for an OAuth 2.0 token now.
pub fn auth_editor(
    ui: &mut Ui,
    auth: &mut Auth,
    inherit_from: Option<&str>,
    show_secrets: &mut bool,
    get_token: &mut bool,
    salt: impl std::hash::Hash + std::fmt::Debug,
) -> bool {
    let mut changed = false;
    let id = egui::Id::unique(("auth_kind", salt));
    ui.horizontal(|ui| {
        fixed(ui, 130.0, |ui| {
            ui.label(RichText::new("Type").family(theme::bold()));
        });
        ComboBox::from_id_salt(id).selected_text(auth.label()).width(220.0).show_ui(ui, |ui| {
            for (label, kind) in Auth::kinds(inherit_from.is_some()) {
                if ui.selectable_label(auth.same_kind(&kind), label).clicked() && !auth.same_kind(&kind) {
                    *auth = kind;
                    changed = true;
                }
            }
        });
        if matches!(auth, Auth::OAuth2(_) | Auth::Basic { .. } | Auth::Bearer { .. } | Auth::ApiKey { .. })
            && ui.small_button(if *show_secrets { "Hide secrets" } else { "Show secrets" }).clicked()
        {
            *show_secrets = !*show_secrets;
        }
    });
    let show = *show_secrets;
    ui.add_space(4.0);
    match auth {
        Auth::Inherit => {
            ui.label(RichText::new(format!("Uses {}.", inherit_from.unwrap_or("the workspace's authentication"))).color(theme::MUTED));
        }
        Auth::None => {
            ui.label(RichText::new("No Authorization header, no key.").color(theme::MUTED));
        }
        Auth::Basic { username, password } => {
            changed |= row(ui, "Username", username, "{{username}}", false, show);
            changed |= row(ui, "Password", password, "{{password}}", true, show);
        }
        Auth::Bearer { token, prefix } => {
            changed |= row(ui, "Token", token, "{{token}}", true, show);
            changed |= row(ui, "Prefix", prefix, "Bearer", false, show);
        }
        Auth::ApiKey { key, value, location } => {
            changed |= row(ui, "Key", key, "X-API-Key", false, show);
            changed |= row(ui, "Value", value, "{{api_key}}", true, show);
            ui.horizontal(|ui| {
                fixed(ui, 130.0, |ui| {
                    ui.label(RichText::new("Add to").color(theme::MUTED));
                });
                changed |= ui.radio_value(location, KeyLocation::Header, "Header").changed();
                changed |= ui.radio_value(location, KeyLocation::Query, "Query parameter").changed();
            });
        }
        Auth::OAuth2(o) => changed |= oauth_editor(ui, o, show, get_token, id),
    }
    ui.add_space(4.0);
    ui.label(
        RichText::new("Every field can use {{variables}} of the environment (keep secrets there, or here: the workspace's go to the system keychain).")
            .size(11.5)
            .color(theme::MUTED),
    );
    changed
}

fn oauth_editor(ui: &mut Ui, o: &mut OAuth2, show: bool, get_token: &mut bool, id: egui::Id) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        fixed(ui, 130.0, |ui| {
            ui.label(RichText::new("Grant").color(theme::MUTED));
        });
        ComboBox::from_id_salt(id.with("grant")).selected_text(o.grant.label()).width(260.0).show_ui(ui, |ui| {
            for g in Grant::ALL {
                changed |= ui.selectable_value(&mut o.grant, g, g.label()).changed();
            }
        });
    });
    if o.grant == Grant::AuthorizationCode {
        changed |= row(ui, "Authorization URL", &mut o.auth_url, "https://example.com/oauth/authorize", false, show);
    }
    changed |= row(ui, "Token URL", &mut o.token_url, "https://example.com/oauth/token", false, show);
    changed |= row(ui, "Client ID", &mut o.client_id, "{{client_id}}", false, show);
    changed |= row(ui, "Client secret", &mut o.client_secret, "{{client_secret}} (empty for public clients)", true, show);
    if o.grant == Grant::Password {
        changed |= row(ui, "Username", &mut o.username, "{{username}}", false, show);
        changed |= row(ui, "Password", &mut o.password, "{{password}}", true, show);
    }
    changed |= row(ui, "Scope", &mut o.scope, "read write (space separated)", false, show);
    changed |= row(ui, "Audience", &mut o.audience, "optional", false, show);
    changed |= row(ui, "Header prefix", &mut o.header_prefix, "Bearer", false, show);
    ui.horizontal(|ui| {
        fixed(ui, 130.0, |ui| {
            ui.label(RichText::new("Client auth").color(theme::MUTED));
        });
        changed |= ui.radio_value(&mut o.client_auth, ClientAuth::Body, "In the body").changed();
        changed |= ui.radio_value(&mut o.client_auth, ClientAuth::Header, "Basic header").changed();
    });
    if o.grant == Grant::AuthorizationCode {
        ui.horizontal(|ui| {
            fixed(ui, 130.0, |ui| {
                ui.label(RichText::new("Callback").color(theme::MUTED));
            });
            changed |= ui.checkbox(&mut o.pkce, "PKCE").changed();
            ui.label(RichText::new("port").color(theme::MUTED));
            changed |= ui.add(egui::DragValue::new(&mut o.redirect_port).range(0..=65535)).on_hover_text("0 = any free port").changed();
            let port = if o.redirect_port == 0 { "<port>".to_owned() } else { o.redirect_port.to_string() };
            ui.label(RichText::new(format!("http://127.0.0.1:{port}/callback")).monospace().size(12.0).color(theme::MUTED));
        });
    }
    ui.collapsing(RichText::new("More fields of the token request").size(12.5), |ui| {
        kv_editor(ui, &mut o.extra, id.with("extra"));
    });
    ui.horizontal(|ui| {
        fixed(ui, 130.0, |_| {});
        if ui.button("Get a token now").on_hover_text("Asks the token endpoint now (the browser opens for the authorization code)").clicked() {
            *get_token = true;
        }
        ui.label(RichText::new("Otherwise one is fetched on the first request, cached and renewed.").size(12.0).color(theme::MUTED));
    });
    changed
}
