use super::*;

impl RuntimeCandidate {
    /// Validate every contributed source export name against the naming rules
    /// before any mutation happens, and derive the bare-name mask table:
    ///
    /// - plugin sources must publish qualified `namespace__local` tool names
    ///   (bare names are reserved for built-in tools) whose namespace head is
    ///   not one of the reserved `mcp`/`harness`/`builtin` planes;
    /// - MCP sources must publish exactly `mcp__<server>__<local>` with valid
    ///   tokens;
    /// - canonical names must be unique across sources (hard error);
    /// - the protected bare name `read` can never be claimed or masked by a
    ///   contributed alias (hard error);
    /// - any other alias collision — with a built-in canonical name, another
    ///   source's canonical name, or another source's alias — becomes an
    ///   explicit mask instead of an error. A source always beats a built-in
    ///   (user scope over the built-in plane), and between two sources the
    ///   lexicographically GREATER source id string wins, which approximates
    ///   "the newer install wins". The winner's tool provides the contested
    ///   name; the loser keeps its qualified canonical spelling as the escape
    ///   hatch.
    ///
    /// All hard conflicts are reported in one grouped diagnostic. The returned
    /// map is the published mask table (contested bare name → winning
    /// canonical name); contested aliases are deliberately not registered as
    /// registry aliases, so the bare plane resolves through compiled views.
    pub(super) fn validate_candidate_source_names(
        &self,
        sources: &[RuntimeSource],
    ) -> Result<BTreeMap<String, String>, RuntimeRefreshError> {
        let (conflicts, masks) = self.adjudicate_source_name_claims(sources);
        if conflicts.is_empty() {
            Ok(masks)
        } else {
            Err(RuntimeRefreshError::NamingConflicts(conflicts.join("\n")))
        }
    }

    /// One claim on a name in the global bare-name plane: `label` is the
    /// source id string (or `built-in`), `canonical` the claimant's canonical
    /// tool name.
    fn record_claim(
        claims: &mut BTreeMap<String, Vec<(String, String)>>,
        name: &str,
        label: &str,
        canonical: &str,
    ) {
        claims
            .entry(name.to_string())
            .or_default()
            .push((label.to_string(), canonical.to_string()));
    }

    /// Validate the candidate source names and adjudicate every bare-name
    /// contest into the mask table. See
    /// [`RuntimeCandidate::validate_candidate_source_names`] for the contract.
    fn adjudicate_source_name_claims(
        &self,
        sources: &[RuntimeSource],
    ) -> (Vec<String>, BTreeMap<String, String>) {
        const RESERVED_HEADS: [&str; 3] = ["mcp", "harness", "builtin"];
        const BUILTIN_LABEL: &str = "built-in";
        let valid_segment = |token: &str| {
            !token.is_empty()
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        };
        let claim_rank = |(label, _): &(String, String)| {
            let source_rank = u8::from(label != BUILTIN_LABEL);
            (source_rank, label.clone())
        };

        let replaced: BTreeSet<String> = sources.iter().map(|s| s.id.to_string()).collect();
        let mut conflicts: Vec<String> = Vec::new();
        // Canonical name → owning source label, for hard duplicate detection
        // and for identifying the built-in remainder of the bare-name plane.
        let mut canonical_owner: BTreeMap<String, String> = BTreeMap::new();
        // Contested-name claims from contributed aliases: alias → claimants.
        let mut alias_claims: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();

        for (id, source) in &self.sources {
            if replaced.contains(&id.to_string()) {
                continue;
            }
            let label = id.to_string();
            for export in &source.exports {
                canonical_owner.insert(export.canonical_name.clone(), label.clone());
            }
        }

        for source in sources {
            let label = source.id.to_string();
            for export in &source.exports {
                let name = export.canonical_name.as_str();
                match source.id.kind {
                    RuntimeSourceKind::Plugin | RuntimeSourceKind::Bundle => {
                        let segments: Vec<&str> = name.split("__").collect();
                        let shaped = segments.len() >= 2
                            && segments.iter().all(|segment| valid_segment(segment))
                            && !RESERVED_HEADS.contains(&segments[0]);
                        if !shaped {
                            conflicts.push(format!(
                                "source {label}: tool name `{name}` must be qualified as \
                                 `namespace__local` with a non-reserved namespace token"
                            ));
                            continue;
                        }
                    }
                    RuntimeSourceKind::Mcp => {
                        let parts: Vec<&str> = name.split("__").collect();
                        let shaped = parts.len() == 3
                            && parts[0] == "mcp"
                            && valid_segment(parts[1])
                            && valid_segment(parts[2]);
                        if !shaped {
                            conflicts.push(format!(
                                "source {label}: MCP tool name `{name}` must be \
                                 `mcp__<server>__<local>` with valid tokens"
                            ));
                            continue;
                        }
                    }
                }
                if let Some(owner) = canonical_owner.get(name) {
                    conflicts.push(format!(
                        "source {label}: canonical name `{name}` is already provided by {owner}"
                    ));
                    continue;
                }
                canonical_owner.insert(name.to_string(), label.clone());
                for alias in &export.aliases {
                    if hya_tool_loader::is_protected_tool_name(alias) {
                        conflicts.push(format!(
                            "source {label}: protected tool `{alias}` cannot be masked; \
                             the alias is rejected"
                        ));
                        continue;
                    }
                    if canonical_owner
                        .get(alias)
                        .is_some_and(|owner| owner == &label)
                    {
                        conflicts.push(format!(
                            "source {label}: alias `{alias}` collides with its own canonical name"
                        ));
                        continue;
                    }
                    let claimants = alias_claims.entry(alias.clone()).or_default();
                    if claimants.iter().any(|(owner, _)| owner == &label) {
                        conflicts.push(format!(
                            "source {label}: alias `{alias}` is declared more than once"
                        ));
                        continue;
                    }
                    claimants.push((label.clone(), export.canonical_name.clone()));
                }
            }
        }
        for (id, source) in &self.sources {
            if replaced.contains(&id.to_string()) {
                continue;
            }
            let label = id.to_string();
            for export in &source.exports {
                for alias in &export.aliases {
                    Self::record_claim(&mut alias_claims, alias, &label, &export.canonical_name);
                }
            }
        }

        // Adjudication: a name contested by more than one claimant resolves by
        // the masking total order — sources beat built-ins, and between two
        // sources the lexicographically greater source id string wins.
        // Canonical names owned by replaced sources still sit in the tool
        // snapshot during validation; they are about to be removed, so they
        // must not pose as built-in claims.
        let replaced_canonicals: BTreeSet<String> = self
            .sources
            .iter()
            .filter(|(id, _)| replaced.contains(&id.to_string()))
            .flat_map(|(_, source)| {
                source
                    .exports
                    .iter()
                    .map(|export| export.canonical_name.clone())
                    .collect::<Vec<_>>()
            })
            .collect();
        let tool_canonicals: BTreeSet<String> = self
            .tools
            .snapshot()
            .canonical_tools()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let mut masks = BTreeMap::new();
        for (name, mut claimants) in alias_claims {
            if let Some(owner) = canonical_owner.get(&name) {
                claimants.push((owner.clone(), name.clone()));
            } else if !replaced_canonicals.contains(&name) && tool_canonicals.contains(&name) {
                claimants.push((BUILTIN_LABEL.to_string(), name.clone()));
            }
            if claimants.len() < 2 {
                continue;
            }
            claimants.sort_by_key(claim_rank);
            if let Some((_, winner)) = claimants.last() {
                masks.insert(name, winner.clone());
            }
        }
        (conflicts, masks)
    }

    /// Recompute the mask table for the candidate's retained sources after a
    /// removal. Retained sources are pre-validated, so this cannot produce
    /// hard conflicts.
    pub(super) fn recompute_masks_after_removal(&mut self) {
        let (_, masks) = self.adjudicate_source_name_claims(&[]);
        self.masks = masks;
    }

    /// Recompute the scheme table for the candidate's retained sources after a
    /// removal. Retained sources are pre-validated, so this cannot produce
    /// hard conflicts.
    pub(super) fn recompute_schemes_after_removal(&mut self) {
        let (_, schemes) = self.adjudicate_source_scheme_claims(&[]);
        self.schemes = schemes;
    }

    /// Validate every contributed source schema claim before any mutation
    /// happens, and derive the published scheme table:
    ///
    /// - the scheme must be a `[a-zA-Z0-9_-]` token of at least two characters
    ///   without the `__` separator (hard error);
    /// - the internal families `artifact`/`skill`/`local` are reserved and can
    ///   never be registered (hard error);
    /// - the claimed `canonical_tool` must be a canonical export of the *same*
    ///   source (hard error), so a source can only bind schemes to tools it
    ///   actually ships. `Bundle` sources are exempt: their tools are
    ///   view-scoped (sidecar activation) rather than registry exports, so a
    ///   bundle claims its owning tool by its `bundle:{id}/tool/{local}`
    ///   stable id instead;
    /// - a scheme claimed twice by one source is a declaration bug (hard
    ///   error);
    /// - a scheme claimed by two different sources resolves by the masking
    ///   total order: the lexicographically GREATER source id string wins,
    ///   approximating "the newer install wins". The table records only the
    ///   winner; the full claim chain stays derivable through
    ///   [`TurnBinding::scheme_chain`].
    ///
    /// All hard violations are reported in one grouped diagnostic.
    pub(super) fn validate_candidate_source_schemes(
        &self,
        sources: &[RuntimeSource],
    ) -> Result<BTreeMap<String, SchemeBinding>, RuntimeRefreshError> {
        let (conflicts, schemes) = self.adjudicate_source_scheme_claims(sources);
        if conflicts.is_empty() {
            Ok(schemes)
        } else {
            Err(RuntimeRefreshError::SchemaConflicts(conflicts.join("\n")))
        }
    }

    /// Collect and adjudicate every scheme claim from the candidate sources
    /// and the retained (non-replaced) sources. See
    /// [`RuntimeCandidate::validate_candidate_source_schemes`] for the contract.
    fn adjudicate_source_scheme_claims(
        &self,
        sources: &[RuntimeSource],
    ) -> (Vec<String>, BTreeMap<String, SchemeBinding>) {
        let replaced: BTreeSet<String> = sources.iter().map(|s| s.id.to_string()).collect();
        let mut conflicts: Vec<String> = Vec::new();
        // Scheme → claimants ordered by publication; the winner is the
        // lexicographically greatest source id (no built-in plane exists for
        // schemes: internal families are reserved outright).
        let mut claims: BTreeMap<String, Vec<(String, SchemeBinding)>> = BTreeMap::new();

        let record_claim =
            |source: &RuntimeSource,
             conflicts: &mut Vec<String>,
             claims: &mut BTreeMap<String, Vec<(String, SchemeBinding)>>| {
                let label = source.id.to_string();
                let mut declared: BTreeSet<&str> = BTreeSet::new();
                for schema in &source.schemas {
                    let scheme = schema.scheme.as_str();
                    if SchemeRegistry::is_internal_scheme(scheme) {
                        conflicts.push(format!(
                            "source {label}: internal scheme `{scheme}` is reserved and cannot \
                         be registered"
                        ));
                        continue;
                    }
                    if !SchemeRegistry::is_valid_scheme_token(scheme) {
                        conflicts.push(format!(
                            "source {label}: scheme `{scheme}` must be a `[a-zA-Z0-9_-]` token \
                         of at least two characters without `__`"
                        ));
                        continue;
                    }
                    // Bundle sources keep their tools view-scoped (sidecar
                    // activation), so their schema claims name the owning
                    // tool by its `bundle:{id}/tool/{local}` stable id rather
                    // than a registry export.
                    if source.id.kind() != RuntimeSourceKind::Bundle
                        && source
                            .exports
                            .iter()
                            .all(|export| export.canonical_name != schema.canonical_tool)
                    {
                        conflicts.push(format!(
                            "source {label}: schema `{scheme}` claims tool `{}` which it does \
                         not export",
                            schema.canonical_tool
                        ));
                        continue;
                    }
                    if !declared.insert(scheme) {
                        conflicts.push(format!(
                            "source {label}: scheme `{scheme}` is declared more than once"
                        ));
                        continue;
                    }
                    claims.entry(scheme.to_string()).or_default().push((
                        label.clone(),
                        SchemeBinding::new(
                            label.clone(),
                            schema.canonical_tool.clone(),
                            schema.writable,
                        ),
                    ));
                }
            };

        for (id, source) in &self.sources {
            if replaced.contains(&id.to_string()) {
                continue;
            }
            record_claim(source, &mut conflicts, &mut claims);
        }
        for source in sources {
            record_claim(source, &mut conflicts, &mut claims);
        }

        // Adjudication: between two sources the lexicographically greater
        // source id string wins, mirroring the bare-name mask order.
        let schemes = claims
            .into_iter()
            .filter_map(|(scheme, mut claimants)| {
                claimants.sort_by(|left, right| left.0.cmp(&right.0));
                claimants.pop().map(|(_, binding)| (scheme, binding))
            })
            .collect();
        (conflicts, schemes)
    }
}
