#![allow(dead_code)]

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::registry::{ImageConfigBlob, Manifest, ManifestResponse};

#[derive(Debug, Clone)]
pub struct LayerInfo {
    pub digest: String,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct ImageDetail {
    pub digest: String,
    pub content_type: String,
    pub created: Option<String>,
    pub os_arch: Option<String>,
    pub layers: Vec<LayerInfo>,
    pub total_size: u64,
    pub pull_url: String,
    pub labels: Vec<(String, String)>,
    pub is_index: bool,
    pub platforms: Vec<String>,
}

impl ImageDetail {
    pub fn from_manifest_and_config(
        resp: &ManifestResponse,
        config: Option<&ImageConfigBlob>,
        repo: &str,
        tag: &str,
        registry_url: &str,
    ) -> Self {
        let host = registry_host(registry_url);
        let pull_url = format!("{host}/{repo}:{tag}");

        match &resp.manifest {
            Manifest::Image(img) => {
                let layers: Vec<LayerInfo> = img
                    .layers
                    .iter()
                    .map(|l| LayerInfo {
                        digest: l.digest.clone(),
                        size: l.size.max(0) as u64,
                    })
                    .collect();
                let total_size: u64 = layers.iter().map(|l| l.size).sum();

                let (created, os_arch, labels) = config
                    .map(|c| {
                        let os_arch = match (&c.os, &c.architecture) {
                            (Some(os), Some(arch)) => Some(format!("{os}/{arch}")),
                            (Some(os), None) => Some(os.clone()),
                            _ => None,
                        };
                        let labels = c
                            .config
                            .labels
                            .clone()
                            .unwrap_or_default()
                            .into_iter()
                            .collect::<Vec<_>>();
                        (c.created.clone(), os_arch, labels)
                    })
                    .unwrap_or_default();

                Self {
                    digest: resp.digest.clone(),
                    content_type: resp.content_type.clone(),
                    created,
                    os_arch,
                    layers,
                    total_size,
                    pull_url,
                    labels,
                    is_index: false,
                    platforms: Vec::new(),
                }
            }
            Manifest::Index(idx) => {
                let platforms: Vec<String> = idx
                    .manifests
                    .iter()
                    .map(|m| {
                        if let Some(p) = &m.platform {
                            let base = format!("{}/{}", p.os, p.architecture);
                            if let Some(v) = &p.variant {
                                format!("{base}/{v}")
                            } else {
                                base
                            }
                        } else {
                            m.digest[..16.min(m.digest.len())].to_owned()
                        }
                    })
                    .collect();

                Self {
                    digest: resp.digest.clone(),
                    content_type: resp.content_type.clone(),
                    created: None,
                    os_arch: None,
                    layers: Vec::new(),
                    total_size: 0,
                    pull_url,
                    labels: Vec::new(),
                    is_index: true,
                    platforms,
                }
            }
        }
    }
}

pub fn format_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if n >= GB {
        format!("{:.1} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
}

fn registry_host(registry_url: &str) -> &str {
    registry_url
        .trim_end_matches('/')
        .trim_start_matches("https://")
        .trim_start_matches("http://")
}

const KEY_STYLE: Style = Style::new().fg(Color::Cyan);
const VAL_STYLE: Style = Style::new().fg(Color::White);
const DIM_STYLE: Style = Style::new().fg(Color::DarkGray);
const URL_STYLE: Style = Style::new().fg(Color::Green).add_modifier(Modifier::BOLD);

pub fn render_lines(detail: &ImageDetail) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    kv(
        &mut lines,
        "Pull URL",
        Span::styled(detail.pull_url.clone(), URL_STYLE),
    );

    let digest_short = if detail.digest.len() > 19 {
        format!("{}…", &detail.digest[..19])
    } else {
        detail.digest.clone()
    };
    kv(
        &mut lines,
        "Digest",
        Span::styled(detail.digest.clone(), VAL_STYLE),
    );
    drop(digest_short);

    kv(
        &mut lines,
        "Type",
        Span::styled(detail.content_type.clone(), DIM_STYLE),
    );

    if detail.is_index {
        kv(
            &mut lines,
            "Kind",
            Span::styled("multi-platform index", VAL_STYLE),
        );
        lines.push(key_line("Platforms"));
        for p in &detail.platforms {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("• {p}"), VAL_STYLE),
            ]));
        }
    } else {
        if let Some(created) = &detail.created {
            kv(
                &mut lines,
                "Created",
                Span::styled(created.clone(), VAL_STYLE),
            );
        }
        if let Some(os_arch) = &detail.os_arch {
            kv(
                &mut lines,
                "OS/Arch",
                Span::styled(os_arch.clone(), VAL_STYLE),
            );
        }

        kv(
            &mut lines,
            "Total size",
            Span::styled(format_bytes(detail.total_size), VAL_STYLE),
        );

        lines.push(key_line(&format!("Layers ({})", detail.layers.len())));
        for (i, layer) in detail.layers.iter().enumerate() {
            let short = if layer.digest.len() > 19 {
                format!("{}…", &layer.digest[..19])
            } else {
                layer.digest.clone()
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("[{i}] "), DIM_STYLE),
                Span::styled(short, DIM_STYLE),
                Span::raw("  "),
                Span::styled(format_bytes(layer.size), VAL_STYLE),
            ]));
        }

        if !detail.labels.is_empty() {
            lines.push(Line::raw(""));
            lines.push(key_line("Labels"));
            let mut sorted = detail.labels.clone();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            for (k, v) in sorted {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(format!("{k}: "), KEY_STYLE),
                    Span::styled(v, VAL_STYLE),
                ]));
            }
        }
    }

    lines
}

fn kv(lines: &mut Vec<Line<'static>>, key: &'static str, val: Span<'static>) {
    lines.push(Line::from(vec![
        Span::styled(format!("{key:<12}"), KEY_STYLE),
        val,
    ]));
}

fn key_line(key: &str) -> Line<'static> {
    Line::from(Span::styled(format!("{key}:"), KEY_STYLE))
}
