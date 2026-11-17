//! Several export folders and archives in one collection: the clone's merge of import results
//! (frontend/src/import/merge.ts) as its import of several sources uses it (multi.ts), and as
//! "add to the import on screen" uses it (App.tsx:483-486).
//!
//! * Posts are taken by id in source order; a later post with an id that is already there takes
//!   the earlier one's place and adds the warning `messages::merged_duplicate` ("read last
//!   wins").
//! * The merged posts are sorted newest first; posts without a time follow in their order.
//! * The warnings of each source are prefixed with its name ("export-basic.zip: ..."), except
//!   those of a collection that was merged before: they say which source they are about already.
//! * One result on its own is returned as it is: no prefix, post.json order.

use std::collections::HashMap;

use crate::manifest::LoadedExport;
use crate::messages;
use crate::model::{fold_path, ExportKind, ExportSource, Post, Warning, WarningCode};

/// One export source that was read: what the collection will say about it, and its posts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedSource {
    /// The source (its `shown` and `replaced` are set by the merge).
    pub source: ExportSource,
    /// Its posts and warnings, as the reader made them.
    pub loaded: LoadedExport,
}

/// One result of a merge: a newly read source, or the collection on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeInput {
    /// What the duplicate sentence calls it: the source's name, or the collection's label.
    pub name: String,
    /// Its posts in its order.
    pub posts: Vec<Post>,
    /// Its warnings.
    pub warnings: Vec<Warning>,
    /// True when the warnings say which source they are about already (a collection that was
    /// merged before); otherwise they are prefixed with `name`.
    pub prefixed: bool,
    /// The sources behind it: one for a newly read source, all of them for the collection on
    /// screen.
    pub sources: Vec<ExportSource>,
}

impl MergeInput {
    /// A newly read source.
    pub fn from_loaded(loaded: LoadedSource) -> MergeInput {
        MergeInput {
            name: loaded.source.name.clone(),
            posts: loaded.loaded.posts,
            warnings: loaded.loaded.warnings,
            prefixed: false,
            sources: vec![loaded.source],
        }
    }
}

/// What a merge produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Merged {
    /// The posts in display order.
    pub posts: Vec<Post>,
    /// Every warning, prefixed with its source.
    pub warnings: Vec<Warning>,
    /// Every source, with `shown` and `replaced` brought up to date.
    pub sources: Vec<ExportSource>,
    /// The entries of all post.json files, less the posts that were replaced.
    pub total_entries: usize,
    /// The source link all sources agree on, if they do.
    pub source_url: Option<String>,
}

/// merge.ts `sortPosts`: newest first; posts without a time after all dated ones, in the order
/// given. The sort is stable: posts of the same moment keep their order.
pub fn sort_posts(posts: Vec<Post>) -> Vec<Post> {
    let (mut dated, undated): (Vec<Post>, Vec<Post>) = posts.into_iter().partition(|post| post.taken_at.is_some());
    dated.sort_by_key(|post| std::cmp::Reverse(post.taken_at));
    dated.extend(undated);
    dated
}

/// merge.ts `describeSources` over the kinds of the sources: archives, and folders (an export
/// folder, the browser imports and the web app's library all count as folders).
pub fn describe_sources(kinds: impl IntoIterator<Item = ExportKind>) -> String {
    let (mut archives, mut folders) = (0, 0);
    for kind in kinds {
        match kind {
            ExportKind::Archive => archives += 1,
            ExportKind::Folder | ExportKind::WebLibrary => folders += 1,
        }
    }
    messages::describe_source_counts(archives, folders, 0)
}

/// `warning` prefixed with the name of its source and marked as being about `source`.
fn from_source(warning: Warning, name: &str, source: &str) -> Warning {
    Warning { message: messages::from_source(name, &warning.message), source: Some(source.to_owned()), ..warning }
}

/// merge.ts `mergeResults` over two or more results (web brief 3.5): see the module comment.
/// A result with a single source marks its warnings with that source (`Warning::source`), so
/// that they can go with its posts when it is opened again.
pub fn merge(inputs: Vec<MergeInput>) -> Merged {
    let names: Vec<String> = inputs.iter().map(|input| input.name.clone()).collect();
    let mut winners: Vec<Post> = Vec::new();
    let mut place: HashMap<String, (usize, usize)> = HashMap::new();
    let mut warnings: Vec<Warning> = Vec::new();
    let mut sources: Vec<ExportSource> = Vec::new();

    for (from, input) in inputs.into_iter().enumerate() {
        let identity = match input.sources.as_slice() {
            [only] => Some(fold_path(only.identity())),
            _ => None,
        };
        for warning in input.warnings {
            warnings.push(match (&identity, input.prefixed) {
                (Some(identity), false) => from_source(warning, &input.name, identity),
                _ => warning,
            });
        }
        let mut replaced = 0;
        for post in input.posts {
            match place.get(&post.id).copied() {
                Some((position, earlier)) => {
                    let mut duplicate =
                        Warning::new(WarningCode::DuplicateId, messages::merged_duplicate(&post.id, &input.name, &names[earlier]));
                    duplicate.source = identity.clone();
                    warnings.push(duplicate);
                    place.insert(post.id.clone(), (position, from));
                    // The newer post takes the older one's place.
                    winners[position] = post;
                    replaced += 1;
                }
                None => {
                    place.insert(post.id.clone(), (winners.len(), from));
                    winners.push(post);
                }
            }
        }
        let mut input_sources = input.sources;
        if let [only] = input_sources.as_mut_slice() {
            if !input.prefixed {
                only.replaced = replaced;
            }
        }
        sources.extend(input_sources);
    }

    let posts = sort_posts(winners);
    for source in &mut sources {
        source.shown = posts.iter().filter(|post| source.owns(post)).count();
    }
    let total_entries =
        sources.iter().map(|source| source.total_entries).sum::<usize>().saturating_sub(sources.iter().map(|source| source.replaced).sum());
    let first_url = sources.first().and_then(|source| source.source_url.clone());
    let source_url = first_url.filter(|url| sources.iter().all(|source| source.source_url.as_deref() == Some(url.as_str())));
    Merged { posts, warnings, sources, total_entries, source_url }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaItem, MediaKind, MediaType, PostKey, PostOrigin, User};
    use std::path::{Path, PathBuf};

    fn post(id: &str, taken_at: Option<i64>, archive: &str) -> Post {
        let archive = PathBuf::from(archive);
        Post {
            key: PostKey::for_archive_post(&archive, id),
            id: id.to_owned(),
            permalink: None,
            media: vec![MediaItem { kind: MediaKind::Image, path: PathBuf::from(format!("{id}.jpg")), manifest_path: format!("posts/{id}.jpg"), poster: None, alt: None, bytes: 1 }],
            user: User::named("tester"),
            caption: format!("from {}", archive.display()),
            original_caption: String::new(),
            comment_count: 0,
            like_count: None,
            taken_at,
            media_type: MediaType::Image,
            alt: None,
            location: None,
            context: None,
            comments: Vec::new(),
            origin: PostOrigin::Archive { archive },
        }
    }

    fn source(name: &str, total_entries: usize, url: Option<&str>) -> ExportSource {
        ExportSource {
            kind: ExportKind::Archive,
            path: PathBuf::from(name),
            manifest: PathBuf::from(format!("{name}-post.json")),
            name: name.to_owned(),
            opened_at: 0,
            exported_at: None,
            source_url: url.map(str::to_owned),
            total_entries,
            shown: 0,
            replaced: 0,
        }
    }

    fn input(name: &str, posts: Vec<Post>, warnings: &[&str]) -> MergeInput {
        let total = posts.len();
        MergeInput {
            name: name.to_owned(),
            posts,
            warnings: warnings.iter().map(|message| Warning::new(WarningCode::InvalidEntry, *message)).collect(),
            prefixed: false,
            sources: vec![source(name, total, None)],
        }
    }

    fn ids(merged: &Merged) -> Vec<&str> {
        merged.posts.iter().map(|post| post.id.as_str()).collect()
    }

    fn messages_of(merged: &Merged) -> Vec<&str> {
        merged.warnings.iter().map(|warning| warning.message.as_str()).collect()
    }

    #[test]
    fn later_results_win_and_say_so() {
        // merge.test.ts "a later result replaces posts with the same id".
        let first = input("one.zip", vec![post("a", Some(3), "one.zip"), post("b", Some(2), "one.zip")], &[]);
        let second = input("two.zip", vec![post("b", Some(5), "two.zip"), post("c", None, "two.zip")], &[]);
        let merged = merge(vec![first, second]);
        assert_eq!(ids(&merged), ["b", "a", "c"]);
        assert_eq!(merged.posts[0].caption, "from two.zip");
        assert_eq!(messages_of(&merged), ["The post \"b\" from two.zip replaces the one with the same id from one.zip."]);
        assert_eq!(merged.warnings[0].code, WarningCode::DuplicateId);
        assert_eq!((merged.sources[0].shown, merged.sources[0].replaced), (1, 0));
        assert_eq!((merged.sources[1].shown, merged.sources[1].replaced), (2, 1));
        assert_eq!(merged.total_entries, 3);
    }

    #[test]
    fn a_replacing_post_keeps_the_place_of_the_old_one_among_undated_posts() {
        let first = input("one.zip", vec![post("a", None, "one.zip"), post("b", None, "one.zip"), post("c", None, "one.zip")], &[]);
        let second = input("two.zip", vec![post("d", None, "two.zip"), post("b", None, "two.zip")], &[]);
        let merged = merge(vec![first, second]);
        assert_eq!(ids(&merged), ["a", "b", "c", "d"]);
        assert_eq!(merged.posts[1].caption, "from two.zip");
    }

    #[test]
    fn posts_are_sorted_newest_first_and_undated_ones_follow_in_order() {
        let posts = vec![post("old", Some(1), "x"), post("none-1", None, "x"), post("new", Some(9), "x"), post("none-2", None, "x"), post("mid", Some(5), "x"), post("mid-2", Some(5), "x")];
        let sorted: Vec<String> = sort_posts(posts).into_iter().map(|post| post.id).collect();
        assert_eq!(sorted, ["new", "mid", "mid-2", "old", "none-1", "none-2"]);
    }

    #[test]
    fn warnings_are_prefixed_with_their_source_once() {
        let first = input("one.zip", vec![post("a", None, "one.zip")], &["Post 2 has no image path and was skipped."]);
        let mut earlier = input("2 archives", vec![post("b", None, "x.zip")], &["x.zip: Already prefixed."]);
        earlier.prefixed = true;
        earlier.sources = vec![source("x.zip", 1, None), source("y.zip", 0, None)];
        let merged = merge(vec![earlier, first]);
        assert_eq!(messages_of(&merged), ["x.zip: Already prefixed.", "one.zip: Post 2 has no image path and was skipped."]);
        assert_eq!(merged.warnings[1].source.as_deref(), Some(fold_path(Path::new("one.zip")).as_str()));
        assert_eq!(merged.warnings[0].source, None);
    }

    #[test]
    fn the_source_link_is_kept_only_when_every_source_agrees() {
        let mut first = input("one.zip", vec![post("a", None, "one.zip")], &[]);
        let mut second = input("two.zip", vec![post("b", None, "two.zip")], &[]);
        first.sources[0].source_url = Some("https://www.instagram.com/lumen.atlas/".to_owned());
        second.sources[0].source_url = Some("https://www.instagram.com/lumen.atlas/".to_owned());
        assert_eq!(merge(vec![first.clone(), second.clone()]).source_url.as_deref(), Some("https://www.instagram.com/lumen.atlas/"));
        second.sources[0].source_url = None;
        assert_eq!(merge(vec![first, second]).source_url, None);
    }

    #[test]
    fn sources_are_described_like_the_clone() {
        assert_eq!(describe_sources([ExportKind::Archive, ExportKind::Archive, ExportKind::Archive]), "3 archives");
        assert_eq!(describe_sources([ExportKind::Archive, ExportKind::Folder, ExportKind::Archive]), "2 archives and 1 folder");
        assert_eq!(describe_sources([ExportKind::WebLibrary, ExportKind::Folder]), "2 folders");
        assert_eq!(describe_sources([]), "Nothing");
    }
}
