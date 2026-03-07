use super::*;

pub(super) fn model(
    _this: &PopoverHost,
    repo_id: RepoId,
    section: BranchSection,
) -> ContextMenuModel {
    let header: SharedString = match section {
        BranchSection::Local => "Local".into(),
        BranchSection::Remote => "Remote".into(),
    };
    let mut items = vec![ContextMenuItem::Header(header)];
    items.push(ContextMenuItem::Separator);
    items.push(ContextMenuItem::Entry {
        label: "Switch branch".into(),
        icon: Some("⎇".into()),
        shortcut: Some("Enter".into()),
        disabled: false,
        action: Box::new(ContextMenuAction::OpenPopover {
            kind: PopoverKind::BranchPicker,
        }),
    });

    if section == BranchSection::Remote {
        items.push(ContextMenuItem::Entry {
            label: "Add remote…".into(),
            icon: Some("+".into()),
            shortcut: None,
            disabled: false,
            action: Box::new(ContextMenuAction::OpenPopover {
                kind: PopoverKind::RemoteAddPrompt { repo_id },
            }),
        });
        items.push(ContextMenuItem::Entry {
            label: "Fetch all".into(),
            icon: Some("↓".into()),
            shortcut: Some("F".into()),
            disabled: false,
            action: Box::new(ContextMenuAction::FetchAll { repo_id }),
        });
        items.push(ContextMenuItem::Entry {
            label: "Prune merged branches".into(),
            icon: Some("🧹".into()),
            shortcut: None,
            disabled: false,
            action: Box::new(ContextMenuAction::PruneMergedBranches { repo_id }),
        });
        items.push(ContextMenuItem::Entry {
            label: "Prune local tags".into(),
            icon: Some("🏷".into()),
            shortcut: None,
            disabled: false,
            action: Box::new(ContextMenuAction::PruneLocalTags { repo_id }),
        });
        items.push(ContextMenuItem::Separator);
        items.push(ContextMenuItem::Entry {
            label: "Delete remote branch…".into(),
            icon: Some("🗑".into()),
            shortcut: None,
            disabled: false,
            action: Box::new(ContextMenuAction::OpenPopover {
                kind: PopoverKind::RemoteBranchDeletePicker {
                    repo_id,
                    remote: None,
                },
            }),
        });
    }

    ContextMenuModel::new(items)
}
