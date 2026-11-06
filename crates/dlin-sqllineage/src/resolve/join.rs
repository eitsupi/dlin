use std::collections::HashSet;

use crate::graph::scope::{JoinMerge, JoinMergeKind, MergedColumns};
use crate::{Dialect, Ident};

/// Fold complete relation outputs in join order. Unknown or invalid join
/// shapes leave expansion to the caller's conservative fallback.
pub(super) fn merge_join_columns<T: Clone>(
    mut groups: Vec<(Vec<Ident>, Vec<T>)>,
    merges: &[JoinMerge],
    dialect: Dialect,
    name: impl Fn(&T) -> &Ident,
    combine: impl Fn(&T, &T, JoinMergeKind) -> T,
) -> Option<Vec<T>> {
    let mut seen = HashSet::new();
    if groups
        .iter()
        .flat_map(|(members, _)| members)
        .any(|member| !seen.insert(member.column_key(dialect).into_owned()))
    {
        return None;
    }
    for merge in merges {
        let side = |members: &[Ident]| {
            groups
                .iter()
                .enumerate()
                .filter_map(|(index, (names, _))| {
                    names
                        .iter()
                        .any(|name| {
                            members.iter().any(|member| {
                                member.column_key(dialect) == name.column_key(dialect)
                            })
                        })
                        .then_some(index)
                })
                .collect::<Vec<_>>()
        };
        let left = side(&merge.left);
        let right = side(&merge.right);
        if left.is_empty() || right.is_empty() || left.iter().any(|i| right.contains(i)) {
            return None;
        }
        let left_columns: Vec<T> = left.iter().flat_map(|&i| groups[i].1.clone()).collect();
        let right_columns: Vec<T> = right.iter().flat_map(|&i| groups[i].1.clone()).collect();
        let keys = match &merge.columns {
            MergedColumns::Using(names) => names.clone(),
            MergedColumns::Natural => left_columns
                .iter()
                .filter(|column| {
                    right_columns.iter().any(|other| {
                        name(column).column_key(dialect) == name(other).column_key(dialect)
                    })
                })
                .map(|column| name(column).clone())
                .collect(),
        };
        let mut output = Vec::new();
        let mut selected_keys = HashSet::new();
        for key in keys {
            let matches = |columns: &[T]| {
                columns
                    .iter()
                    .enumerate()
                    .filter_map(|(i, column)| {
                        (name(column).column_key(dialect) == key.column_key(dialect)).then_some(i)
                    })
                    .collect::<Vec<_>>()
            };
            let l = matches(&left_columns);
            let r = matches(&right_columns);
            let ([l], [r]) = (l.as_slice(), r.as_slice()) else {
                return None;
            };
            if !selected_keys.insert(key.column_key(dialect).into_owned()) {
                return None;
            }
            output.push(combine(&left_columns[*l], &right_columns[*r], merge.kind));
        }
        output.extend(
            left_columns
                .into_iter()
                .chain(right_columns)
                .filter(|column| {
                    !selected_keys.contains(name(column).column_key(dialect).as_ref())
                }),
        );
        let members = left
            .iter()
            .chain(&right)
            .flat_map(|&i| groups[i].0.clone())
            .collect();
        let first = *left.iter().chain(&right).min()?;
        let mut index = 0;
        groups.retain(|_| {
            let keep = !left.contains(&index) && !right.contains(&index);
            index += 1;
            keep
        });
        groups.insert(first, (members, output));
    }
    Some(
        groups
            .into_iter()
            .flat_map(|(_, columns)| columns)
            .collect(),
    )
}
