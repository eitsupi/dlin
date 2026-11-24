//! Call-time rendering of the string selected by var().

use super::*;
use crate::parser::jinja::interp::convert::{VarTemplateParseError, parse_var_template};

// Bound the syntax held on the evaluation stack across a chain. A long
// binary expression can keep all of its left-hand calls on that stack.
const MAX_ACTIVE_TEMPLATE_TOKENS: usize = 256;

pub(super) struct VarFrame {
    pub(super) name: String,
    pub(super) call_line: Option<u32>,
    configured: bool,
}

impl Interp<'_> {
    pub(super) fn render_var_template(
        &mut self,
        caller: &State,
        name: String,
        text: &str,
        configured: bool,
        span: Span,
    ) -> Ev<AValue> {
        let recursive = configured
            && self
                .var_frames
                .iter()
                .any(|frame| frame.configured && frame.name == name);
        // An interpreter has one resolved configuration scope for the node.
        // Defaults do not identify a configured value in that scope.
        let call_span = self.root_call.unwrap_or(span);
        let call_line = (call_span.source == SourceId::Template && call_span.line != 0)
            .then_some(call_span.line);
        self.var_frames.push(VarFrame {
            name,
            call_line,
            configured,
        });
        let root_call = self.root_call.take();
        let root_scopes = std::mem::take(&mut self.root_scopes);
        let current = self.current.replace(Span {
            source: SourceId::VarTemplate,
            line: 0,
            start: 0,
            end: 0,
        });
        let result = self.eval_var_template(caller, text, recursive);
        if matches!(result, Err(Signal::Budget)) && self.budget_site.is_none() {
            self.budget_site = self.current.map(|span| self.site(span));
        }
        self.current = current;
        self.root_scopes = root_scopes;
        self.root_call = root_call;
        self.var_frames.pop();
        result
    }

    fn eval_var_template(&mut self, caller: &State, text: &str, recursive: bool) -> Ev<AValue> {
        let span = self.current.expect("var template has a current span");
        let reason = if recursive {
            DependencyReason::CyclicVarTemplate {
                at: self.site(span),
            }
        } else if self.depth >= MAX_DEPTH {
            DependencyReason::AnalysisLimitReached {
                limit: AnalysisLimit::RecursionDepth,
                at: self.site(span),
            }
        } else {
            // Charge the parser input against the shared work budget as well
            // as enforcing the per-template parser limits.
            self.charge(text.len() as u64, None)?;
            let available_tokens = MAX_ACTIVE_TEMPLATE_TOKENS - self.var_template_tokens;
            match parse_var_template(text, available_tokens) {
                Ok((template, token_count)) => {
                    let frame = State::new(
                        vec![Scope::new()],
                        Heap::default(),
                        Output::Known(String::new()),
                        caller.path.clone(),
                    );
                    self.depth += 1;
                    self.var_template_tokens += token_count;
                    let result = self.exec_block(frame, &template.body);
                    self.var_template_tokens -= token_count;
                    self.depth -= 1;
                    let flow = result?;
                    if flow.ret.is_some() || flow.brk.is_some() || flow.cont.is_some() {
                        DependencyReason::JinjaRenderFailed {
                            at: self.site(self.current.unwrap_or(span)),
                        }
                    } else if let Some(state) = flow.normal {
                        return Ok(match state.out {
                            Output::Known(text) => AValue::Known(Value::from(text)),
                            output => output.into_value(),
                        });
                    } else {
                        return Err(Signal::Abort);
                    }
                }
                Err(VarTemplateParseError::Budget) => DependencyReason::AnalysisLimitReached {
                    limit: AnalysisLimit::VarTemplateParseBudget,
                    at: self.site(span),
                },
                Err(VarTemplateParseError::Syntax(failure)) => {
                    DependencyReason::JinjaRenderFailed {
                        at: self.site(Span {
                            line: failure.line.unwrap_or_default(),
                            ..span
                        }),
                    }
                }
            }
        };
        let causes = Causes::one(reason);
        self.record_used(caller, &causes);
        Ok(AValue::Unknown(causes))
    }
}
