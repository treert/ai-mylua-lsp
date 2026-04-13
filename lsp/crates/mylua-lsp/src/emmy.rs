/// Lightweight EmmyLua annotation parser.
/// Extracts structured annotations from `---` comment text.

#[derive(Debug, Clone)]
pub enum EmmyAnnotation {
    Class { name: String, parents: Vec<String>, desc: String },
    Field { visibility: Option<String>, name: String, type_text: String, desc: String },
    Param { name: String, type_text: String, desc: String },
    Return { type_text: String, name: Option<String>, desc: String },
    Type { type_text: String, desc: String },
    Alias { name: String, type_text: String },
    Generic { params: String },
    Deprecated { desc: String },
    Other { tag: String, text: String },
}

pub fn parse_emmy_comments(comment_text: &str) -> Vec<EmmyAnnotation> {
    let mut annotations = Vec::new();

    for line in comment_text.lines() {
        let line = line.trim();
        let content = if let Some(rest) = line.strip_prefix("---") {
            rest.trim()
        } else if let Some(rest) = line.strip_prefix("--") {
            rest.trim()
        } else {
            continue;
        };

        if let Some(rest) = content.strip_prefix('@') {
            if let Some(ann) = parse_annotation(rest) {
                annotations.push(ann);
            }
        }
    }

    annotations
}

fn parse_annotation(text: &str) -> Option<EmmyAnnotation> {
    let (tag, rest) = split_first_word(text);
    match tag {
        "class" => {
            let (name, rest) = split_first_word(rest);
            let parents = if let Some(rest) = rest.strip_prefix(':') {
                rest.trim()
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            } else {
                Vec::new()
            };
            Some(EmmyAnnotation::Class {
                name: name.to_string(),
                parents,
                desc: rest.to_string(),
            })
        }
        "field" => {
            let rest = rest.trim();
            let (visibility, rest) = match rest.split_once(char::is_whitespace) {
                Some((first, rest2))
                    if matches!(first, "public" | "protected" | "private" | "package") =>
                {
                    (Some(first.to_string()), rest2.trim())
                }
                _ => (None, rest),
            };
            let (name, rest) = split_first_word(rest);
            let (type_text, desc) = split_first_word(rest);
            Some(EmmyAnnotation::Field {
                visibility,
                name: name.to_string(),
                type_text: type_text.to_string(),
                desc: desc.to_string(),
            })
        }
        "param" => {
            let (name, rest) = split_first_word(rest);
            let name = name.trim_end_matches('?');
            let (type_text, desc) = split_first_word(rest);
            Some(EmmyAnnotation::Param {
                name: name.to_string(),
                type_text: type_text.to_string(),
                desc: desc.to_string(),
            })
        }
        "return" => {
            let (type_text, rest) = split_first_word(rest);
            let (maybe_name, desc) = split_first_word(rest);
            let name = if !maybe_name.is_empty()
                && maybe_name.chars().next().map_or(false, |c| c.is_alphabetic() || c == '_')
                && !maybe_name.contains('|')
            {
                Some(maybe_name.to_string())
            } else {
                None
            };
            Some(EmmyAnnotation::Return {
                type_text: type_text.to_string(),
                name,
                desc: desc.to_string(),
            })
        }
        "type" => {
            let (type_text, desc) = split_first_word(rest);
            Some(EmmyAnnotation::Type {
                type_text: type_text.to_string(),
                desc: desc.to_string(),
            })
        }
        "alias" => {
            let (name, type_text) = split_first_word(rest);
            Some(EmmyAnnotation::Alias {
                name: name.to_string(),
                type_text: type_text.to_string(),
            })
        }
        "generic" => Some(EmmyAnnotation::Generic {
            params: rest.to_string(),
        }),
        "deprecated" => Some(EmmyAnnotation::Deprecated {
            desc: rest.to_string(),
        }),
        _ => Some(EmmyAnnotation::Other {
            tag: tag.to_string(),
            text: rest.to_string(),
        }),
    }
}

fn split_first_word(s: &str) -> (&str, &str) {
    let s = s.trim();
    match s.find(char::is_whitespace) {
        Some(pos) => (&s[..pos], s[pos..].trim_start()),
        None => (s, ""),
    }
}

/// Collect EmmyLua comment lines immediately before a given node.
/// Checks for both:
///   - `emmy_comment` nodes (structured, from grammar) containing `emmy_line` children
///   - Legacy `comment` tokens starting with `---` (fallback)
pub fn collect_preceding_comments<'a>(
    node: tree_sitter::Node<'a>,
    source: &'a [u8],
) -> Vec<String> {
    let mut comments = Vec::new();
    let mut sibling = node.prev_sibling();

    while let Some(prev) = sibling {
        match prev.kind() {
            "emmy_comment" => {
                let mut lines = Vec::new();
                for i in 0..prev.named_child_count() {
                    if let Some(line_node) = prev.named_child(i as u32) {
                        if line_node.kind() == "emmy_line" {
                            lines.push(line_node.utf8_text(source).unwrap_or("").to_string());
                        }
                    }
                }
                // emmy_comment children are ordered, but we're walking siblings backward
                comments.extend(lines.into_iter().rev());
                sibling = prev.prev_sibling();
                continue;
            }
            "comment" => {
                let text = prev.utf8_text(source).unwrap_or("");
                if text.starts_with("---") {
                    comments.push(text.to_string());
                    sibling = prev.prev_sibling();
                    continue;
                }
            }
            _ => {}
        }
        break;
    }

    comments.reverse();
    comments
}

/// Format EmmyLua annotations as Markdown for Hover display.
pub fn format_annotations_markdown(annotations: &[EmmyAnnotation]) -> String {
    let mut parts = Vec::new();

    for ann in annotations {
        match ann {
            EmmyAnnotation::Param { name, type_text, desc } => {
                let mut s = format!("@param `{}` `{}`", name, type_text);
                if !desc.is_empty() {
                    s.push_str(&format!(" — {}", desc));
                }
                parts.push(s);
            }
            EmmyAnnotation::Return { type_text, name, desc } => {
                let mut s = format!("@return `{}`", type_text);
                if let Some(n) = name {
                    s.push_str(&format!(" `{}`", n));
                }
                if !desc.is_empty() {
                    s.push_str(&format!(" — {}", desc));
                }
                parts.push(s);
            }
            EmmyAnnotation::Type { type_text, desc } => {
                let mut s = format!("@type `{}`", type_text);
                if !desc.is_empty() {
                    s.push_str(&format!(" — {}", desc));
                }
                parts.push(s);
            }
            EmmyAnnotation::Class { name, parents, desc } => {
                let mut s = format!("@class `{}`", name);
                if !parents.is_empty() {
                    s.push_str(&format!(" : {}", parents.join(", ")));
                }
                if !desc.is_empty() {
                    s.push_str(&format!(" — {}", desc));
                }
                parts.push(s);
            }
            EmmyAnnotation::Field { name, type_text, desc, .. } => {
                let mut s = format!("@field `{}` `{}`", name, type_text);
                if !desc.is_empty() {
                    s.push_str(&format!(" — {}", desc));
                }
                parts.push(s);
            }
            EmmyAnnotation::Deprecated { desc } => {
                let mut s = "@deprecated".to_string();
                if !desc.is_empty() {
                    s.push_str(&format!(" {}", desc));
                }
                parts.push(s);
            }
            _ => {}
        }
    }

    parts.join("\n\n")
}
