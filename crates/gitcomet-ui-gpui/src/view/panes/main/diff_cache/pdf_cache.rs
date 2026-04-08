use super::image_cache::cached_image_diff_path;
use super::*;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

fn file_pdf_preview_signature(file: &gitcomet_core::domain::FileDiffImage) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    file.path.hash(&mut hasher);
    file.old.hash(&mut hasher);
    file.new.hash(&mut hasher);
    hasher.finish()
}

fn build_file_pdf_diff_preview(file: &gitcomet_core::domain::FileDiffImage) -> PdfDiffPreview {
    if file.old.is_some() && file.old == file.new {
        let content = build_pdf_preview_content(file.old.as_deref());
        return PdfDiffPreview {
            old: content.clone(),
            new: content,
        };
    }

    std::thread::scope(|scope| {
        let old_task = file
            .old
            .as_deref()
            .map(|bytes| scope.spawn(move || build_pdf_preview_content(Some(bytes))));
        let new_task = file
            .new
            .as_deref()
            .map(|bytes| scope.spawn(move || build_pdf_preview_content(Some(bytes))));

        PdfDiffPreview {
            old: old_task
                .map(|task| task.join().unwrap_or(PdfPreviewContent::Missing))
                .unwrap_or(PdfPreviewContent::Missing),
            new: new_task
                .map(|task| task.join().unwrap_or(PdfPreviewContent::Missing))
                .unwrap_or(PdfPreviewContent::Missing),
        }
    })
}

fn build_pdf_preview_content(bytes: Option<&[u8]>) -> PdfPreviewContent {
    let Some(bytes) = bytes.filter(|bytes| !bytes.is_empty()) else {
        return PdfPreviewContent::Missing;
    };

    match cached_pdf_document_preview(bytes) {
        Ok(document) => PdfPreviewContent::Ready(document),
        Err(error) => PdfPreviewContent::Error(error.to_string().into()),
    }
}

fn cached_pdf_document_preview(bytes: &[u8]) -> std::io::Result<Arc<PdfDocumentPreview>> {
    let pdf_path = cached_image_diff_path(bytes, "pdf").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            "Failed to persist PDF preview payload",
        )
    })?;

    Ok(Arc::new(PdfDocumentPreview { pdf_path }))
}

impl MainPaneView {
    pub(in crate::view) fn ensure_file_pdf_preview_cache(&mut self, cx: &mut gpui::Context<Self>) {
        let clear_cache = |this: &mut Self| {
            this.file_pdf_preview_cache_repo_id = None;
            this.file_pdf_preview_cache_target = None;
            this.file_pdf_preview_cache_rev = 0;
            this.file_pdf_preview_cache_content_signature = None;
            this.file_pdf_preview = Loadable::NotLoaded;
            this.file_pdf_preview_inflight = None;
        };

        let Some((repo_id, diff_file_rev, diff_target, file)) = (|| {
            let repo = self.active_repo()?;
            if !Self::is_file_diff_target(repo.diff_state.diff_target.as_ref()) {
                return None;
            }
            if crate::view::diff_target_rendered_preview_kind(repo.diff_state.diff_target.as_ref())
                != Some(RenderedPreviewKind::Pdf)
            {
                return None;
            }

            let file = match &repo.diff_state.diff_file_image {
                Loadable::Ready(Some(file)) => Some(Arc::clone(file)),
                _ => None,
            };

            Some((
                repo.id,
                repo.diff_state.diff_file_rev,
                repo.diff_state.diff_target.clone(),
                file,
            ))
        })() else {
            clear_cache(self);
            return;
        };

        let diff_target_for_task = diff_target.clone();
        let file_content_signature = file.as_ref().map(|file| file_pdf_preview_signature(file));
        let same_repo_and_target = self.file_pdf_preview_cache_repo_id == Some(repo_id)
            && self.file_pdf_preview_cache_target == diff_target;

        if same_repo_and_target && self.file_pdf_preview_cache_rev == diff_file_rev {
            return;
        }

        if same_repo_and_target
            && let Some(signature) = file_content_signature
            && self.file_pdf_preview_cache_content_signature == Some(signature)
        {
            if self.file_pdf_preview_inflight.is_none() {
                self.file_pdf_preview_cache_rev = diff_file_rev;
            }
            return;
        }

        self.file_pdf_preview_cache_repo_id = Some(repo_id);
        self.file_pdf_preview_cache_rev = diff_file_rev;
        self.file_pdf_preview_cache_content_signature = None;
        self.file_pdf_preview_cache_target = diff_target;
        self.file_pdf_preview = Loadable::NotLoaded;
        self.file_pdf_preview_inflight = None;

        let Some(file) = file else {
            return;
        };
        let content_signature =
            file_content_signature.unwrap_or_else(|| file_pdf_preview_signature(file.as_ref()));

        self.file_pdf_preview = Loadable::Loading;
        self.file_pdf_preview_seq = self.file_pdf_preview_seq.wrapping_add(1);
        let seq = self.file_pdf_preview_seq;
        self.file_pdf_preview_inflight = Some(seq);

        cx.spawn(
            async move |view: WeakEntity<MainPaneView>, cx: &mut gpui::AsyncApp| {
                let preview =
                    smol::unblock(move || build_file_pdf_diff_preview(file.as_ref())).await;

                let _ = view.update(cx, |this, cx| {
                    if this.file_pdf_preview_inflight != Some(seq) {
                        return;
                    }
                    if this.file_pdf_preview_cache_repo_id != Some(repo_id)
                        || this.file_pdf_preview_cache_rev != diff_file_rev
                        || this.file_pdf_preview_cache_target != diff_target_for_task
                    {
                        return;
                    }

                    this.file_pdf_preview_inflight = None;
                    this.file_pdf_preview_cache_content_signature = Some(content_signature);
                    this.file_pdf_preview = Loadable::Ready(Arc::new(preview));
                    cx.notify();
                });
            },
        )
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_file_pdf_diff_preview_persists_pdf_payloads_for_external_viewers() {
        let file = gitcomet_core::domain::FileDiffImage {
            path: std::path::PathBuf::from("docs/spec.pdf"),
            old: Some(b"%PDF-1.7\nnot-a-real-pdf\n".to_vec()),
            new: Some(b"%PDF-1.7\nnot-a-real-pdf\n".to_vec()),
        };

        let preview = build_file_pdf_diff_preview(&file);
        assert!(matches!(
            preview.old,
            PdfPreviewContent::Ready(ref document) if document.pdf_path.exists()
        ));
        assert!(matches!(
            preview.new,
            PdfPreviewContent::Ready(ref document) if document.pdf_path.exists()
        ));
    }
}
