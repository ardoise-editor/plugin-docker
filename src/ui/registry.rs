//! The tab of a registry: its repositories, and the tags of one.

use ardoise_plugin::ui::*;
use rust_i18n::t;

use super::{error_line, heading, labelled, muted, table};
use crate::registry::HUB;
use crate::state::{App, RegistryView};

pub fn registry(app: &App, view: &RegistryView) -> Node {
    let logged = app.data.logged_in.contains(&view.host);
    let mut nodes = vec![
        row([
            heading(crate::actions::registry_name(&view.host)),
            muted(if logged {
                t!("docker.logged_in")
            } else {
                t!("docker.not_logged_in")
            }),
            fill(),
            labelled("docker.login", "rg.login", "import"),
            labelled("docker.logout", "rg.logout", "x").enabled(logged),
        ])
        .spacing(8.0),
        row([
            input("reg.typed", &view.typed, t!("docker.repository_hint")).on_submit("reg.tags"),
            button(t!("docker.show_tags"), "reg.tags").enabled(!view.typed.trim().is_empty()),
        ])
        .spacing(6.0),
    ];
    // Only Docker Hub is searched; the others list what they hold.
    if view.host == HUB {
        nodes.push(
            row([
                input("reg.search", &view.search, t!("docker.search_hub")).on_submit("reg.find"),
                button(t!("docker.search"), "reg.find").enabled(!view.search.trim().is_empty()),
            ])
            .spacing(6.0),
        );
    }
    nodes.extend(error_line(&view.error));
    if view.loading {
        nodes.push(muted(t!("docker.loading")));
    }
    if !view.repositories.is_empty() {
        let titles = [
            t!("docker.repository").into_owned(),
            t!("docker.information").into_owned(),
        ];
        let rows: Vec<Vec<String>> = view
            .repositories
            .iter()
            .map(|(name, said)| vec![name.clone(), said.clone()])
            .collect();
        let mut found = table("repositories", &titles, &rows);
        for (column, width) in found.columns.iter_mut().zip([280.0, 600.0]) {
            column.width = Some(width);
        }
        nodes.push(Node::from(found).height(220.0));
    } else if !view.unlisted.is_empty() {
        nodes.push(muted(format!(
            "{} ({})",
            t!("docker.registry_unlisted"),
            view.unlisted
        )));
    }
    if let Some(repository) = &view.repository {
        let picked = view.tag.is_some_and(|row| row < view.tags.len());
        nodes.push(
            row([
                heading(format!("{repository} — {}", view.tags.len())),
                fill(),
                labelled("docker.pull", "reg.pull", "download").enabled(picked),
                labelled("docker.run_image", "reg.run", "play").enabled(picked),
                labelled("docker.copy_name", "reg.copy", "copy").enabled(picked),
            ])
            .spacing(6.0),
        );
        let rows: Vec<Vec<String>> = view.tags.iter().map(|t| vec![t.clone()]).collect();
        let mut tags = table("tags", &[t!("docker.tag_name").into_owned()], &rows);
        tags.selection = view.tag.map(|row| GridSelection {
            anchor: (row, 0),
            cursor: (row, 0),
        });
        for column in &mut tags.columns {
            column.width = Some(360.0);
        }
        tags.menu = vec![
            MenuItem::new(t!("docker.pull"), "reg.pull"),
            MenuItem::new(t!("docker.run_image"), "reg.run"),
            MenuItem::new(t!("docker.copy_name"), "reg.copy"),
        ];
        nodes.push(tags.into());
    }
    column(nodes).spacing(8.0).padding(10.0)
}
