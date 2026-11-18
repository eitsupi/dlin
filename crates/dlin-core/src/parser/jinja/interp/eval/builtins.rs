//! Dependency registration and dbt built-in functions.

use super::*;

/// Prefix dbt reserves for secret environment variables, which it only
/// allows in profiles and packages.
const SECRET_ENV_PREFIX: &str = "DBT_ENV_SECRET";

impl<'a> Interp<'a> {
    pub(super) fn string_arg(
        &self,
        st: &State,
        value: &AValue,
        span: Span,
    ) -> Result<Option<String>, Causes> {
        match self.freeze(st, value, span)? {
            value if value.kind() == ValueKind::String => Ok(value.as_str().map(str::to_string)),
            _ => Ok(None),
        }
    }

    pub(super) fn reject(
        &mut self,
        st: &State,
        function: DependencyFunction,
        causes: Causes,
        call: &Call,
    ) {
        self.record_used(st, &causes);
        let mut found = StaticCalls::default();
        let mut names = Vec::new();
        for arg in &call.args {
            found.literals(arg.expr());
        }
        super::super::ir::collect_names(
            &[],
            &call
                .args
                .iter()
                .map(|a| a.expr().clone())
                .collect::<Vec<_>>(),
            &mut names,
        );
        if !names.is_empty() {
            let library = self.library;
            let template_literals = self.template_set_literals();
            for name in &names {
                if let Some(literals) = template_literals.get(name.as_ref()) {
                    found.strings.extend(literals.iter().cloned());
                }
                if let Some(literals) = library.set_literals.get(name.as_ref()) {
                    found.strings.extend(literals.iter().cloned());
                }
            }
        }
        self.rec.decisions.push(Decision {
            function,
            reference: None,
            source: None,
            literals: found.strings,
            line: call.span.line,
            outcome: DecisionOutcome::Rejected,
        });
    }

    /// String literals assigned to each variable in the template, computed
    /// on the first rejection.
    pub(super) fn template_set_literals(&mut self) -> &HashMap<Arc<str>, Vec<String>> {
        if self.template_literals.is_none() {
            let mut literals = HashMap::new();
            if let Some(body) = self.template {
                collect_set_literals(body, &mut literals);
            }
            self.template_literals = Some(literals);
        }
        self.template_literals.as_ref().expect("initialised above")
    }

    pub(super) fn invalid(&self, function: DependencyFunction, span: Span) -> Causes {
        Causes::one(DependencyReason::InvalidDependencyCall {
            function,
            at: self.site(span),
        })
    }

    pub(super) fn builtin(
        &mut self,
        st: &mut State,
        builtin: Builtin,
        args: Vec<AValue>,
        kwargs: Vec<(Arc<str>, AValue)>,
        call: &Call,
    ) -> Ev<AValue> {
        let span = call.span;
        let kwarg = |name: &str| {
            kwargs
                .iter()
                .find(|(k, _)| k.as_ref() == name)
                .map(|(_, v)| v)
        };
        match builtin {
            Builtin::Ref => {
                let function = DependencyFunction::Ref;
                if args.is_empty() || args.len() > 2 {
                    let causes = self.invalid(function, span);
                    self.reject(st, function, causes, call);
                    return Ok(AValue::Relation);
                }
                let mut parts = Vec::new();
                let mut unknown: Option<Causes> = None;
                let mut invalid = false;
                for value in &args {
                    match self.string_arg(st, value, span) {
                        Ok(Some(text)) => parts.push(text),
                        Ok(None) => invalid = true,
                        Err(causes) => {
                            unknown = Some(unknown.map_or(causes.clone(), |c| c.union(&causes)))
                        }
                    }
                }
                for (key, value) in &kwargs {
                    match self.freeze(st, value, span) {
                        // An unknown `**kwargs` may supply any keyword.
                        Err(causes)
                            if key.as_ref() == "**" || matches!(value, AValue::Unknown(_)) =>
                        {
                            unknown = Some(unknown.map_or(causes.clone(), |c| c.union(&causes)))
                        }
                        _ if !matches!(key.as_ref(), "version" | "v") => invalid = true,
                        _ => {}
                    }
                }
                let version = match kwarg("version").or_else(|| kwarg("v")) {
                    None => None,
                    Some(value) => match self.freeze(st, value, span) {
                        Ok(v) if v.kind() == ValueKind::Number => Some(v.to_string()),
                        Ok(v) if v.kind() == ValueKind::String => {
                            v.as_str().map(normalize_version_str)
                        }
                        Ok(v) if v.is_none() => None,
                        Ok(_) => {
                            invalid = true;
                            None
                        }
                        Err(causes) => {
                            unknown = Some(unknown.map_or(causes.clone(), |c| c.union(&causes)));
                            None
                        }
                    },
                };
                if let Some(causes) = unknown {
                    self.reject(st, function, causes, call);
                } else if invalid {
                    let causes = self.invalid(function, span);
                    self.reject(st, function, causes, call);
                } else {
                    let (package, name) = match parts.len() {
                        1 => (None, parts.remove(0)),
                        _ => {
                            let name = parts.remove(1);
                            (Some(parts.remove(0)), name)
                        }
                    };
                    let reference = RefCall {
                        package,
                        name,
                        version,
                    };
                    self.accept(st, Some(reference), None, span);
                }
                Ok(AValue::Relation)
            }
            Builtin::Source => {
                let function = DependencyFunction::Source;
                if args.len() != 2 || !kwargs.is_empty() {
                    let causes = kwargs
                        .iter()
                        .filter_map(|(_, value)| value.causes().cloned())
                        .reduce(|a, b| a.union(&b))
                        .unwrap_or_else(|| self.invalid(function, span));
                    self.reject(st, function, causes, call);
                    return Ok(AValue::Relation);
                }
                match (
                    self.string_arg(st, &args[0], span),
                    self.string_arg(st, &args[1], span),
                ) {
                    (Ok(Some(source_name)), Ok(Some(table_name))) => {
                        self.accept(
                            st,
                            None,
                            Some(SourceCall {
                                source_name,
                                table_name,
                            }),
                            span,
                        );
                    }
                    (Err(a), Err(b)) => self.reject(st, function, a.union(&b), call),
                    (Err(causes), _) | (_, Err(causes)) => self.reject(st, function, causes, call),
                    _ => {
                        let causes = self.invalid(function, span);
                        self.reject(st, function, causes, call);
                    }
                }
                Ok(AValue::Relation)
            }
            Builtin::Var => {
                let Some(name) = args.first() else {
                    return Ok(AValue::undefined());
                };
                let name = match self.string_arg(st, name, span) {
                    Ok(Some(name)) => name,
                    Ok(None) => return Ok(AValue::undefined()),
                    Err(causes) => return Ok(AValue::Unknown(causes)),
                };
                if let Some(value) = self.vars.get(&name) {
                    return Ok(AValue::Known(Value::from(minijinja::value::Serde(value))));
                }
                if let Some(default) = args.get(1).or_else(|| kwarg("default")) {
                    return Ok(default.clone());
                }
                Ok(AValue::Unknown(Causes::one(
                    DependencyReason::UnresolvedVar {
                        name: Some(name),
                        at: self.site(span),
                    },
                )))
            }
            Builtin::EnvVar => {
                // The environment is never read, so the result does not depend
                // on the machine: a default is taken like `var()`'s, and a
                // variable without one stays unknown.
                let Some(name) = args.first() else {
                    return Ok(AValue::undefined());
                };
                let name = match self.string_arg(st, name, span) {
                    Ok(Some(name)) => name,
                    Ok(None) => return Ok(AValue::undefined()),
                    Err(causes) => return Ok(AValue::Unknown(causes)),
                };
                // dbt refuses secret variables outside profiles and packages,
                // so their defaults do not stand for a value dbt would use.
                if !name.starts_with(SECRET_ENV_PREFIX)
                    && let Some(default) = args.get(1).or_else(|| kwarg("default"))
                {
                    return Ok(default.clone());
                }
                Ok(AValue::Unknown(self.runtime(
                    RuntimeValueSource::EnvVar,
                    Some(name),
                    span,
                )))
            }
            Builtin::Config => {
                if self.record_config {
                    let conditional = !st.path.is_empty();
                    if let Some(value) = kwarg("materialized") {
                        let materialized = self.string_arg(st, value, span).ok().flatten();
                        // A later call overrides an earlier one. A value set
                        // on an unknown path is only kept when it is the value
                        // already in place.
                        if !conditional {
                            self.rec.materialized_unknown = materialized.is_none();
                            self.rec.config.materialized = materialized;
                        } else if materialized.is_none()
                            || materialized != self.rec.config.materialized
                        {
                            self.rec.materialized_unknown = true;
                            self.rec.config.materialized = None;
                        }
                    }
                    // Tags accumulate. Tags set on an unknown path are left
                    // out rather than claimed.
                    if !conditional
                        && let Some(value) = kwarg("tags")
                        && let Ok(tags) = self.freeze(st, value, span)
                    {
                        let tags: Vec<String> = match tags.as_str() {
                            Some(tag) => vec![tag.to_string()],
                            None => tags
                                .try_iter()
                                .map(|iter| iter.map(|tag| tag.to_string()).collect())
                                .unwrap_or_default(),
                        };
                        for tag in tags {
                            if !self.rec.config.tags.contains(&tag) {
                                self.rec.config.tags.push(tag);
                            }
                        }
                    }
                }
                Ok(AValue::Known(Value::from("")))
            }
            Builtin::ConfigGet => {
                let name = args
                    .first()
                    .and_then(|value| self.string_arg(st, value, span).ok().flatten());
                Ok(AValue::Unknown(self.runtime(
                    RuntimeValueSource::ConfigValue,
                    name,
                    span,
                )))
            }
            Builtin::IsIncremental => Ok(AValue::Known(Value::from(self.incremental))),
            Builtin::Return => Err(Signal::Return(
                args.into_iter().next().unwrap_or_else(AValue::none),
            )),
            Builtin::Noop | Builtin::Statement => Ok(AValue::Known(Value::from(""))),
            Builtin::RunQuery => Ok(AValue::Unknown(self.runtime(
                RuntimeValueSource::RunQuery,
                None,
                span,
            ))),
            Builtin::LoadResult => Ok(AValue::Unknown(self.runtime(
                RuntimeValueSource::Statement,
                None,
                span,
            ))),
            Builtin::Namespace => {
                let mut attrs = BTreeMap::new();
                if let Some(AValue::Heap(id)) = args.first()
                    && let Some(HeapObj::Map(entries)) = st.heap.get(id)
                {
                    for (key, value) in entries {
                        if let Some(key) = key.as_str() {
                            attrs.insert(Arc::from(key), value.clone());
                        }
                    }
                }
                for (key, value) in kwargs {
                    attrs.insert(key, value);
                }
                Ok(self.alloc(st, HeapObj::Namespace(attrs)))
            }
            Builtin::Minijinja(name) => {
                let mut values = match self.freeze_all(st, &args, span) {
                    Ok(values) => values,
                    Err(causes) => return Ok(AValue::Unknown(causes)),
                };
                if !kwargs.is_empty() {
                    let mut pairs = Vec::new();
                    for (key, value) in &kwargs {
                        match self.freeze(st, value, span) {
                            Ok(value) if key.as_ref() != "**" => {
                                pairs.push((key.to_string(), value))
                            }
                            Ok(_) => {
                                return Ok(AValue::Unknown(self.unsupported("**kwargs", span)));
                            }
                            Err(causes) => return Ok(AValue::Unknown(causes)),
                        }
                    }
                    values.push(Value::from(minijinja::value::Kwargs::from_iter(pairs)));
                }
                match Ok::<_, Causes>(values) {
                    Ok(values) => Ok(known::call_global(name, &values)
                        .map(AValue::Known)
                        .unwrap_or_else(|_| {
                            AValue::Unknown(self.unsupported(format!("function:{name}"), span))
                        })),
                    Err(causes) => Ok(AValue::Unknown(causes)),
                }
            }
            Builtin::Unmodelled(name) => Ok(AValue::Unknown(
                self.unsupported(format!("function:{name}"), span),
            )),
            Builtin::Dispatch => {
                let name = args
                    .first()
                    .and_then(|value| self.string_arg(st, value, span).ok().flatten());
                let namespace = kwarg("macro_namespace")
                    .or_else(|| args.get(1))
                    .and_then(|value| self.string_arg(st, value, span).ok().flatten());
                let causes = Causes::one(DependencyReason::UnsupportedDispatch {
                    name,
                    namespace,
                    at: self.site(span),
                });
                self.record_used(st, &causes);
                Ok(AValue::Unknown(causes))
            }
            Builtin::RaiseCompilerError => Err(Signal::Abort),
        }
    }

    pub(super) fn accept(
        &mut self,
        st: &State,
        reference: Option<RefCall>,
        source: Option<SourceCall>,
        span: Span,
    ) {
        let conditional = !st.path.is_empty();
        self.rec.add_causes(&st.path);
        let (function, literals) = match (&reference, &source) {
            (Some(r), _) => (DependencyFunction::Ref, vec![r.name.clone()]),
            (_, Some(s)) => (
                DependencyFunction::Source,
                vec![s.source_name.clone(), s.table_name.clone()],
            ),
            _ => return,
        };
        let at = self.site(span);
        if let Some(r) = &reference {
            self.rec.add_ref(r.clone(), at.clone());
        }
        if let Some(s) = &source {
            self.rec.add_source(s.clone(), at);
        }
        self.rec.decisions.push(Decision {
            function,
            reference,
            source,
            literals,
            line: span.line,
            outcome: DecisionOutcome::Accepted { conditional },
        });
    }
}
