mod apply;
mod cli;
mod config_merge;
mod discover;

pub(crate) fn run(raw_args: Vec<String>) -> anyhow::Result<()> {
    let args = cli::parse_args(raw_args)?;
    if args.help {
        cli::print_help();
        return Ok(());
    }
    let hya_config_path = args
        .hya_config
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing --hya-config"))?;
    let hya_skills_root = args
        .hya_skills_root
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing --hya-skills-root"))?;

    if args.prune {
        return apply::prune(hya_config_path, hya_skills_root);
    }

    let compat_config_path = args
        .compat_config
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing --compat-config"))?;
    let config = discover::load_compat_config(compat_config_path)?;
    let skill_roots = discover::collect_skill_roots(&args, &config);
    let skills = discover::collect_skills(&skill_roots)?;
    let mcp = discover::collect_supported_mcp(&config);

    if args.dry_run {
        apply::print_dry_run(hya_config_path, hya_skills_root, &mcp, &skills);
        return Ok(());
    }

    apply::apply(hya_config_path, hya_skills_root, &mcp, &skills)
}
