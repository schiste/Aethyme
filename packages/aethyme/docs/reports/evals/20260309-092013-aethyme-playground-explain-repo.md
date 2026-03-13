# Eval Report: Explain this repo

Last Updated: 2026-03-09

- Repository: `/Users/christophehenner/Downloads/Repositories/Aethyme Playground`
- Generated: `2026-03-09T09:20:13.257562+00:00`

## Summary

- Control prompt chars: `176`
- Explore prompt chars: `1164`
- Leverage prompt chars: `111`
- Navigation items: `5`
- Risk items: `961`

## Repo Signals

```json
{
  "boundary_clarity": {
    "score": 68,
    "level": "mixed",
    "evidence": [
      "cross-area semantic edges: 41827/264266",
      "source files with area assignment: 5350/5369",
      "generic source file names: 13"
    ]
  },
  "entrypoint_clarity": {
    "score": 100,
    "level": "strong",
    "evidence": [
      "direct code entrypoint edges: 1192",
      "configs with entrypoints: 6",
      "areas with ambiguous entrypoints: 1"
    ]
  },
  "config_hygiene": {
    "score": 21,
    "level": "weak",
    "evidence": [
      "operational configs: 39",
      "linked configs: 39/39",
      "duplicate config families: 26"
    ]
  },
  "hidden_coupling": {
    "score": 23,
    "level": "weak",
    "evidence": [
      "low-confidence semantic edges: 209164/235410",
      "high-confidence semantic edges: 14639/235410",
      "cross-area semantic edges: 30948/235410"
    ]
  },
  "parser_visibility": {
    "score": 87,
    "level": "strong",
    "evidence": [
      "supported source files: 5099/5369",
      "source files with semantic extraction: 3816/5369",
      "total extracted functions/classes: 16018"
    ]
  }
}
```

## Control

### Prompt

```text
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Aethyme Playground
Explore the repository directly and produce a structured explanation.
```

### Run Metrics

- input tokens: `null`
- output tokens: `null`
- retries: `null`
- wall time: `null`

### Final Output Message

```text
Control runner not executed.
```

### Structured Output

```json
null
```

## Explore

### Prompt

```text
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Aethyme Playground
Explore the repository and produce a structured explanation.

You have access to the following graph navigation commands.
Use them via Bash to explore the repository graph:

  cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli repo inspect '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' --json-output
  cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph overview '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' --json-output
  cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph expand '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' <anchor-id> --json-output

Return only the required structured output.
```

### Run Metrics

- input tokens: `null`
- output tokens: `null`
- retries: `null`
- wall time: `null`

### Final Output Message

```text
Explore runner not executed.
```

### Structured Output

```json
null
```

## Leverage

### Prompt

```text
Task: Explain this repo
Use `AETHYME_EVAL_NAVIGATION_CONTEXT_FILE`.
Return only the required structured output.
```

### Run Metrics

- input tokens: `null`
- output tokens: `null`
- retries: `null`
- wall time: `null`

### Final Output Message

```text
Leverage runner not executed.
```

### Structured Output

```json
null
```

## Comparison

| Metric | Control | Explore | Leverage |
| --- | --- | --- | --- |
| Prompt chars | `176` | `1164` | `111` |

- Navigation items surfaced: `5`
- Risk items surfaced: `961`

## Reference

### Output Schema

```json
{
  "type": "object",
  "additionalProperties": false,
  "required": [
    "repo_summary",
    "code_areas",
    "reference_areas",
    "entrypoints",
    "important_docs",
    "key_configs",
    "key_languages",
    "high_risk_areas",
    "navigation_order",
    "representative_code_files",
    "representative_docs",
    "evidence"
  ],
  "properties": {
    "repo_summary": {
      "type": "string"
    },
    "code_areas": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "reference_areas": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "entrypoints": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "important_docs": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "key_configs": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "key_languages": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "high_risk_areas": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "navigation_order": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "representative_code_files": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "representative_docs": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "evidence": {
      "type": "array",
      "items": {
        "type": "string"
      }
    }
  }
}
```

### Scoring Rubric

```json
{
  "weights": {
    "code_areas": 20,
    "reference_areas": 10,
    "entrypoints": 20,
    "important_docs": 15,
    "key_configs": 10,
    "key_languages": 10,
    "high_risk_areas": 5,
    "navigation_order": 5,
    "representative_code_files": 3,
    "representative_docs": 2
  },
  "notes": [
    "Prefer exact path and area matches.",
    "Navigation order is partial-credit and ordered.",
    "Repo summary is informative but not currently machine-scored."
  ]
}
```

### Reference Output

```json
{
  "repo_summary": "Task: Explain this repo",
  "code_areas": [
    "backend",
    "packages",
    "scripts"
  ],
  "reference_areas": [
    "docs",
    "test-results"
  ],
  "entrypoints": [
    "apps/customer/menu.config.ts",
    "apps/customer/sw.ts",
    "apps/mordor/menu.config.ts"
  ],
  "important_docs": [
    ".claude/README.md",
    "Agents/Skills Manager/README.md",
    "Agents/skills/README.md"
  ],
  "key_configs": [
    "backend/pyproject.toml",
    "packages/auth/package.json"
  ],
  "key_languages": [
    "javascript",
    "python",
    "typescript"
  ],
  "high_risk_areas": [
    ".gcloud_access_token",
    ".github/workflows/migrations-guard.yml",
    ".pnpm-store/v10/index/17/3659f9b86de57d0529eeccc33dc3015026947d415796e549a93f9473012b3d-oauth4webapi@3.8.2.json"
  ],
  "navigation_order": [
    ".claude/README.md",
    "Agents/Skills Manager/README.md",
    "backend",
    "packages",
    "docs"
  ],
  "representative_code_files": [
    "Agents/skills/_meta/scripts/add_frontmatter.py",
    "Agents/skills/_meta/scripts/analyze_repo.py",
    "Agents/skills/_meta/scripts/analyze_usage_logs.py"
  ],
  "representative_docs": [
    ".claude/README.md",
    "Agents/Skills Manager/README.md",
    "Agents/skills/README.md"
  ],
  "evidence": [
    "Agents/skills/_meta/scripts/add_frontmatter.py",
    "Agents/skills/_meta/scripts/analyze_repo.py",
    ".claude/README.md"
  ]
}
```

## Navigation Context

```json
{
  "mode": "iterative_navigation",
  "repo_path": "/Users/christophehenner/Downloads/Repositories/Aethyme Playground",
  "tool_repo_path": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme",
  "tool_python": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python",
  "task": "Explain this repo",
  "anchors": {
    "task": "Explain this repo",
    "anchors": [
      {
        "kind": "file",
        "id": "tools/mcp-mordor/README.md",
        "file": "tools/mcp-mordor/README.md",
        "reason": "repository readme"
      },
      {
        "kind": "file",
        "id": "docs/adr/010-monorepo-architecture.md",
        "file": "docs/adr/010-monorepo-architecture.md",
        "reason": "architecture document"
      },
      {
        "kind": "folder",
        "id": "packages",
        "file": null,
        "reason": "top-level area"
      },
      {
        "kind": "folder",
        "id": "tools",
        "file": null,
        "reason": "top-level area"
      },
      {
        "kind": "file",
        "id": "Agents/skills/_meta/scripts/add_frontmatter.py",
        "file": "Agents/skills/_meta/scripts/add_frontmatter.py",
        "reason": "likely entrypoint"
      }
    ]
  },
  "scope": {
    "task": "Explain this repo",
    "navigation_order": [
      "tools/mcp-mordor/README.md",
      "docs/adr/010-monorepo-architecture.md",
      "packages",
      "tools",
      "Agents/skills/_meta/scripts/add_frontmatter.py"
    ],
    "in_scope_files": [
      "tools/mcp-mordor/README.md"
    ],
    "in_scope_symbols": [],
    "in_scope_areas": [
      "packages",
      "tools"
    ],
    "out_of_scope": [],
    "risks": [
      ".gcloud_access_token",
      ".github/workflows/migrations-guard.yml",
      ".pnpm-store/v10/index/17/3659f9b86de57d0529eeccc33dc3015026947d415796e549a93f9473012b3d-oauth4webapi@3.8.2.json",
      ".pnpm-store/v10/index/18/7b8344ed764b2a6ed9c57bd1dd5d900d845265c7827b6bcdba6f381f90cbee-comma-separated-tokens@1.0.8.json",
      ".pnpm-store/v10/index/29/afbd4ebbadbfb1bc33a593e927a2456cfbf762b9a84a881841b35ca84013ac-class-variance-authority@0.7.1.json",
      ".pnpm-store/v10/index/45/d2547e5704ddc5332a232a420b02bb4e853eef5474824ed1b7986cf8473789-js-tokens@4.0.0.json",
      ".pnpm-store/v10/index/55/dffd1150e2bba3cf26df72021eaba193fa125d711eb76f2151a3c81b074744-@csstools+css-tokenizer@3.0.4.json",
      ".pnpm-store/v10/index/59/dee61cf43ff33cba423edfe13e3abe0ddaa28afc7ec9099ba8366728f4eb8a-@auth+core@0.41.0.json",
      ".pnpm-store/v10/index/9b/16bd13d21314eb746da9f78fa2f93298f07a01b3ea505098cd4826459e0591-js-tokens@9.0.1.json",
      ".pnpm-store/v10/index/a3/69ee27ce43e04491c9b877cdb0390e5d4e7b5edf4592fefd0d7b6f5a90752f-@auth0+auth0-react@2.5.0.json",
      ".pnpm-store/v10/index/ab/f25255dd4ba6dce17f96e4626e286f88963e3c742a245edec44504dad5a9b2-space-separated-tokens@1.1.5.json",
      ".pnpm-store/v10/index/e1/7bf1d84e0dd808abaf5469f8a39e8dd0dba63e4b9df2ed359fd368e768ed56-@auth0+auth0-spa-js@2.5.0.json",
      ".pnpm-store/v10/index/f9/ce7582ab8cdc5ea73159a802eb1127b448a18d0ae13b3d1c20b0cb2fc14687-next-auth@5.0.0-beta.30.json",
      ".pnpm-store/v10/index/ff/b05db84885788349ee695cf22466aa9d2c0f0d9ada50056a18a0fd11a9a67e-eslint-plugin-no-secrets@2.2.1.json",
      ".secrets.baseline",
      "Agents/skills/auth/SKILL.md",
      "Agents/skills/auth/references/api-endpoints.md",
      "Agents/skills/auth/references/api-keys.md",
      "Agents/skills/auth/references/authentication.md",
      "Agents/skills/auth/references/common-patterns.md",
      "Agents/skills/auth/references/database-tables.md",
      "Agents/skills/auth/references/decisions.md",
      "Agents/skills/auth/references/learn-log.md",
      "Agents/skills/auth/references/rbac.md",
      "Agents/skills/auth/references/rbac.md",
      "Agents/skills/auth/references/security.md",
      "Agents/skills/auth/references/troubleshooting.md",
      "Agents/skills/ci-deploy/SKILL.md",
      "Agents/skills/ci-deploy/references/advanced-pipelines.md",
      "Agents/skills/ci-deploy/references/decisions.md",
      "Agents/skills/ci-deploy/references/docker.md",
      "Agents/skills/ci-deploy/references/gcp.md",
      "Agents/skills/ci-deploy/references/kubernetes.md",
      "Agents/skills/ci-deploy/references/learn-log.md",
      "Agents/skills/ci-deploy/references/pipelines.md",
      "Agents/skills/ci-deploy/references/secrets.md",
      "Agents/skills/ci-deploy/references/secrets.md",
      "Agents/skills/database/references/migrations.md",
      "Agents/skills/integrations/references/oauth-flows.md",
      "Agents/tasks/2025-01-13-integrations-onboarding-oauth.md",
      "Agents/tasks/celery-cloudbuild-deploy.md",
      "Agents/tasks/celery-redis-secret-wiring.md",
      "Agents/tasks/dedicated-repo-migration.md",
      "Agents/tasks/fix-bootstrap-permission-case.md",
      "Agents/tasks/fix-environment-discovery-migration.md",
      "Agents/tasks/fix-mordor-roles-permissions-404.md",
      "Agents/tasks/fix-preauth-error-production.md",
      "Agents/tasks/google-oauth-onboarding.md",
      "Agents/tasks/merge-environment-0036-migrations.md",
      "Agents/tasks/otel-step1-deployment.md",
      "Agents/tasks/rbac-implementation-plan-intake.md",
      "Agents/tasks/rbac-pr5-pr8.md",
      "Agents/tasks/rbac-role-management-cleanup.md",
      "Agents/tasks/rbac-role-management-permissions.md",
      "Agents/tasks/role-management-permissions-check.md",
      "apps/customer/src/entry-authenticated.tsx",
      "apps/mordor/src/entry-authenticated.tsx",
      "apps/organizations/src/entry-authenticated.tsx",
      "backend/MIGRATION_SCRIPT.py",
      "backend/accounts/admin_rbac_api_views.py",
      "backend/accounts/admin_rbac_views.py",
      "backend/accounts/auth0_management.py",
      "backend/accounts/auth_analytics_models.py",
      "backend/accounts/auth_analytics_serializers.py",
      "backend/accounts/auth_analytics_views.py",
      "backend/accounts/management/commands/rbac_dump_casl_catalog.py",
      "backend/accounts/management/commands/rbac_lifecycle_tick.py",
      "backend/accounts/management/commands/rbac_roles_summary.py",
      "backend/accounts/management/commands/rbac_seed_permissions.py",
      "backend/accounts/middleware_auth_enforcement.py",
      "backend/accounts/middleware_rbac_identity.py",
      "backend/accounts/migrations/0001_initial.py",
      "backend/accounts/migrations/0002_organization.py",
      "backend/accounts/migrations/0003_userprofile_org_default.py",
      "backend/accounts/migrations/0004_rls_userprofile.py",
      "backend/accounts/migrations/0005_tenant_membership.py",
      "backend/accounts/migrations/0006_userprofile_tenant_nullable.py",
      "backend/accounts/migrations/0007_seed_default_tenants_assign.py",
      "backend/accounts/migrations/0008_userprofile_tenant_nonnull.py",
      "backend/accounts/migrations/0009_rls_userprofile_tenant_update.py",
      "backend/accounts/migrations/0010_alter_userprofile_organization_and_more.py",
      "backend/accounts/migrations/0011_profile_identity_fields.py",
      "backend/accounts/migrations/0012_profile_phone_split.py",
      "backend/accounts/migrations/0013_team_and_identity_extras.py",
      "backend/accounts/migrations/0014_team_id_default.py",
      "backend/accounts/migrations/0015_userprofile_notification_prefs.py",
      "backend/accounts/migrations/0016_userprofile_tz_locale_notif_state.py",
      "backend/accounts/migrations/0017_tenant_notification_policy.py",
      "backend/accounts/migrations/0018_tenant_lifecycle_and_admin_models.py",
      "backend/accounts/migrations/0019_plan_entitlements.py",
      "backend/accounts/migrations/0020_alter_plandefinition_id_and_more.py",
      "backend/accounts/migrations/0021_internal_scopes_and_profile_flag.py",
      "backend/accounts/migrations/0022_custom_attributes.py",
      "backend/accounts/migrations/0023_team_user_custom.py",
      "backend/accounts/migrations/0024_rbac_registry.py",
      "backend/accounts/migrations/0024_rbac_registry.py",
      "backend/accounts/migrations/0025_role_archive.py",
      "backend/accounts/migrations/0025_search_trgm_indexes.py",
      "backend/accounts/migrations/0026_alter_customattributedefinition_id.py",
      "backend/accounts/migrations/0027_merge_20250922_0837.py",
      "backend/accounts/migrations/0028_permission_meta.py",
      "backend/accounts/migrations/0028_permission_meta.py",
      "backend/accounts/migrations/0028_role_risk_fields.py",
      "backend/accounts/migrations/0029_permission_metadata.py",
      "backend/accounts/migrations/0029_permission_metadata.py",
      "backend/accounts/migrations/0030_userprofile_ui_prefs.py",
      "backend/accounts/migrations/0031_enable_tenant_rls.py",
      "backend/accounts/migrations/0032_organization_hierarchy.py",
      "backend/accounts/migrations/0033_remove_organization_org_parent_idx_and_more.py",
      "backend/accounts/migrations/0034_check_constraints.py",
      "backend/accounts/migrations/0035_organization_profile_fields.py",
      "backend/accounts/migrations/0036_grc_organization_fields.py",
      "backend/accounts/migrations/0037_remove_sso_mfa_fields.py",
      "backend/accounts/migrations/0038_alter_organization_tax_id.py",
      "backend/accounts/migrations/0039_tenant_api_calls_month_tenant_api_calls_today_and_more.py",
      "backend/accounts/migrations/0040_tenant_admin_notification_message_and_more.py",
      "backend/accounts/migrations/0041_rolev2_organization_parent_userprofile_primary_team_and_more.py",
      "backend/accounts/migrations/0042_tenanthealthalertrule_tenanthealthmetric_and_more.py",
      "backend/accounts/migrations/0043_broadcasttemplate_scheduledbroadcast_and_more.py",
      "backend/accounts/migrations/0044_alter_scopedpermission_resource_roletemplate.py",
      "backend/accounts/migrations/0044_alter_scopedpermission_resource_roletemplate.py",
      "backend/accounts/migrations/0045_rolev2_tags.py",
      "backend/accounts/migrations/0046_remove_business_unit_and_update_team_types.py",
      "backend/accounts/migrations/0047_sync_rbac_permissions.py",
      "backend/accounts/migrations/0047_sync_rbac_permissions.py",
      "backend/accounts/migrations/0048_remove_userprofile_role.py",
      "backend/accounts/migrations/0049_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/0049_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/0999_rename_rolev2_to_role.py",
      "backend/accounts/migrations/1000_alter_role_options_alter_role_tenant.py",
      "backend/accounts/migrations/1001_drop_legacy_rbac_tables.py",
      "backend/accounts/migrations/1001_drop_legacy_rbac_tables.py",
      "backend/accounts/migrations/1002_recreate_role_permissions_through_table.py",
      "backend/accounts/migrations/1002_recreate_role_permissions_through_table.py",
      "backend/accounts/migrations/1003_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1003_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1004_alter_scopedpermission_action.py",
      "backend/accounts/migrations/1004_alter_scopedpermission_action.py",
      "backend/accounts/migrations/1005_add_device_and_session_models.py",
      "backend/accounts/migrations/1006_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1006_alter_scopedpermission_resource.py",
      "backend/accounts/migrations/1007_add_dashboard_resource.py",
      "backend/accounts/migrations/1008_entitlements_catalog.py",
      "backend/accounts/migrations/1009_seed_owner_internal.py",
      "backend/accounts/migrations/1010_subscription_split.py",
      "backend/accounts/migrations/1011_alter_catalogsubscription_id_alter_creditgrant_id_and_more.py",
      "backend/accounts/migrations/1012_merge_20251105_2056.py",
      "backend/accounts/migrations/1013_delete_rolev2_remove_role_archived_and_more.py",
      "backend/accounts/migrations/1014_notification_columns_and_locale_fields.py",
      "backend/accounts/migrations/1015_merge_20251122_2008.py",
      "backend/accounts/migrations/1016_add_account_models.py",
      "backend/accounts/migrations/1017_assign_demo_admin.py",
      "backend/accounts/migrations/1018_remove_demo_fullaccess_prod.py",
      "backend/accounts/migrations/1019_add_integrations_permissions.py",
      "backend/accounts/migrations/1019_add_integrations_permissions.py",
      "backend/accounts/migrations/1020_add_user_search_trgm_indexes.py",
      "backend/accounts/migrations/1021_role_risk_level_role_risk_meta_and_more.py",
      "backend/accounts/migrations/1022_userprofile_rls_by_user_id.py",
      "backend/accounts/migrations/1023_standardize_rls_gucs.py",
      "backend/accounts/migrations/1024_account_assetentity_fk.py",
      "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
      "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
      "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
      "backend/accounts/migrations/1026_remove_account_accounts_ac_tenant__auth_idx_and_more.py",
      "backend/accounts/migrations/1026_remove_account_accounts_ac_tenant__auth_idx_and_more.py",
      "backend/accounts/migrations/1027_add_risk_rules_permissions.py",
      "backend/accounts/migrations/1027_add_risk_rules_permissions.py",
      "backend/accounts/migrations/1028_add_account_risk_fields.py",
      "backend/accounts/migrations/1029_add_finding_template_model.py",
      "backend/accounts/migrations/1030_role_is_template_role_source_template_and_more.py",
      "backend/accounts/migrations/1031_role_templates_global.py",
      "backend/accounts/migrations/1032_remove_role_accounts_role_template_requires_null_tenant_and_more.py",
      "backend/accounts/migrations/1033_drop_scopedpermission_and_legacy_role_fields.py",
      "backend/accounts/migrations/1033_drop_scopedpermission_and_legacy_role_fields.py",
      "backend/accounts/migrations/1034_alter_userroleassignment_scope_type_and_more.py",
      "backend/accounts/migrations/1035_roleriskpolicy.py",
      "backend/accounts/migrations/1036_account_oauth_scopes.py",
      "backend/accounts/migrations/1036_account_oauth_scopes.py",
      "backend/accounts/migrations/1037_add_external_avatar_url.py",
      "backend/accounts/migrations/1038_grant_demo_admin_v3.py",
      "backend/accounts/migrations/1039_rbac_homogenization.py",
      "backend/accounts/migrations/1039_rbac_homogenization.py",
      "backend/accounts/migrations/1040_remove_permissionauditlog_actor_and_more.py",
      "backend/accounts/migrations/1040_remove_permissionauditlog_actor_and_more.py",
      "backend/accounts/migrations/1041_alter_role_permissions_v3.py",
      "backend/accounts/migrations/1041_alter_role_permissions_v3.py",
      "backend/accounts/migrations/1042_migrate_permissions_to_canonical.py",
      "backend/accounts/migrations/1042_migrate_permissions_to_canonical.py",
      "backend/accounts/migrations/1043_access_grants_and_scope_types.py",
      "backend/accounts/migrations/1044_tenant_slug_global_unique.py",
      "backend/accounts/migrations/1045_rename_accounts_acc_grantor_status_idx_accounts_ac_grantor_970445_idx_and_more.py",
      "backend/accounts/migrations/1046_tenant_onboarding_apps_score_and_more.py",
      "backend/accounts/migrations/1047_tenant_dns_discovery_seed_fields.py",
      "backend/accounts/migrations/1048_authanalyticssummary_authevent.py",
      "backend/accounts/migrations/1048_authanalyticssummary_authevent.py",
      "backend/accounts/migrations/1048_seed_free_plan.py",
      "backend/accounts/migrations/1049_add_domain_role_exposure.py",
      "backend/accounts/migrations/1050_change_domain_role_to_roles_array.py",
      "backend/accounts/migrations/1051_tenant_profiles.py",
      "backend/accounts/migrations/1052_seed_tenant_profiles.py",
      "backend/accounts/migrations/1053_tenant_profile_templates.py",
      "backend/accounts/migrations/1054_seed_tenant_profile_templates.py",
      "backend/accounts/migrations/1055_role_templates_scope_and_profiles.py",
      "backend/accounts/migrations/1056_alter_role_organization_and_more.py",
      "backend/accounts/migrations/1057_tenantdomain_asset_entity.py",
      "backend/accounts/migrations/1058_role_template_visibility_and_auto_create.py",
      "backend/accounts/migrations/1059_fix_account_asset_fk_constraint.py",
      "backend/accounts/migrations/1060_enforce_userprofile_rls.py",
      "backend/accounts/migrations/1061_external_groups.py",
      "backend/accounts/migrations/1062_rename_accounts_ex_tenant__3a632a_idx_accounts_ex_tenant__0c1f4d_idx_and_more.py",
      "backend/accounts/migrations/1063_role_is_platform_staff.py",
      "backend/accounts/migrations/1064_platform_roles.py",
      "backend/accounts/migrations/1065_usersession_realm_enforcement.py",
      "backend/accounts/migrations/1066_remove_platformroleassignment_platform_role_assignment_user_role_uniq_and_more.py",
      "backend/accounts/migrations/1067_consolidate_data_models.py",
      "backend/accounts/migrations/1068_alter_organization_options_alter_team_options_and_more.py",
      "backend/accounts/migrations/1069_documentslot_and_status.py",
      "backend/accounts/migrations/1070_platform_role_assignment_starts_at.py",
      "backend/accounts/migrations/1071_merge_20260202_1350.py",
      "backend/accounts/migrations/1072_seed_default_platform_roles.py",
      "backend/accounts/migrations/1073_feature_key_allow_dots.py",
      "backend/accounts/migrations/1074_aeptus_support_access.py",
      "backend/accounts/migrations/1075_alter_usertenantmembership_role.py",
      "backend/accounts/migrations/1076_userprofile_rls_insert_policy.py",
      "backend/accounts/migrations/1077_migrate_platform_roles_to_canonical_rbac.py",
      "backend/accounts/migrations/1077_migrate_platform_roles_to_canonical_rbac.py",
      "backend/accounts/migrations/1078_userprofile_rls_include_memberships.py",
      "backend/accounts/migrations/1079_userprofile_archived_at.py",
      "backend/accounts/migrations/1080_profile_integrity_jobs.py",
      "backend/accounts/migrations/1081_impersonation_ticket_and_request_id.py",
      "backend/accounts/migrations/1082_alter_tenant_options.py",
      "backend/accounts/migrations/1083_alter_scheduledbroadcast_status.py",
      "backend/accounts/migrations/1084_tenant_profile_fk_and_framework_template.py",
      "backend/accounts/migrations/1085_seed_baseline_framework_templates.py",
      "backend/accounts/migrations/1086_merge_20260305_1932.py",
      "backend/accounts/migrations/FOLDER.migrations.md",
      "backend/accounts/migrations/__init__.py",
      "backend/accounts/permissions_base.py",
      "backend/accounts/rbac.py",
      "backend/accounts/rbac_audit_models.py",
      "backend/accounts/rbac_canonical.py",
      "backend/accounts/rbac_helpers.py",
      "backend/accounts/rbac_models.py",
      "backend/accounts/rbac_permissions.py",
      "backend/accounts/rbac_scope.py",
      "backend/accounts/rbac_signals.py",
      "backend/accounts/tests/test_rbac_access_engine.py",
      "backend/accounts/tests/test_rbac_lifecycle_tick.py",
      "backend/accounts/tests/test_rbac_on_behalf_audit.py",
      "backend/accounts/tests/test_rbac_team_auto_assign.py",
      "backend/adn/migrations/0001_initial.py",
      "backend/adn/migrations/0002_enable_rls.py",
      "backend/adn/migrations/0003_fix_category_slug_uniqueness.py",
      "backend/adn/migrations/0004_pipelinerun_enrichmentqueue_directorysignal_and_more.py",
      "backend/adn/migrations/0005_localproviderentry_localserviceentry_and_more.py",
      "backend/adn/migrations/0006_remove_localproviderentry_unique_local_provider_domain_per_tenant_and_more.py",
      "backend/adn/migrations/0007_add_schema_version.py",
      "backend/adn/migrations/0008_directorycategory_expected_at_onboarding.py",
      "backend/adn/migrations/0009_add_app_metadata_facts.py",
      "backend/adn/migrations/0010_expand_fact_types.py",
      "backend/adn/migrations/0011_add_category_owner_fields.py",
      "backend/adn/migrations/0012_pipelinerun_add_adn_onboarding_enrich_stage.py",
      "backend/adn/migrations/0013_category_owner_delegation.py",
      "backend/adn/migrations/0014_pipelinestageconfig.py",
      "backend/adn/migrations/0015_remove_directoryfact_fact_single_target_entity_and_more.py",
      "backend/adn/migrations/0016_sitemap_supply_chain_choice_expansions.py",
      "backend/adn/migrations/0017_rename_enrichmentqueue_pipelinequeue.py",
      "backend/adn/migrations/0018_rename_adn_pipelin_target__70e8a1_idx_adn_pipelin_target__d13f85_idx_and_more.py",
      "backend/adn/migrations/0019_pipelinebatch.py",
      "backend/adn/migrations/0020_rename_adn_pipelin_status_batch_idx_adn_pipelin_status_90c11e_idx_and_more.py",
      "backend/adn/migrations/__init__.py",
      "backend/adn/permissions.py",
      "backend/adn/tests/test_permissions.py",
      "backend/ai_providers/migrations/0001_initial.py",
      "backend/ai_providers/migrations/0002_seed_providers.py",
      "backend/ai_providers/migrations/__init__.py",
      "backend/analytics/migrations/0001_initial.py",
      "backend/analytics/migrations/__init__.py",
      "backend/api_keys/migrations/0001_initial.py",
      "backend/api_keys/migrations/0002_rename_api_keys_tenant__a3f8b1_idx_api_keys_tenant__aa40c3_idx_and_more.py",
      "backend/api_keys/migrations/0003_unique_constraints.py",
      "backend/api_keys/migrations/0004_apikey_user.py",
      "backend/api_keys/migrations/__init__.py",
      "backend/api_usage/migrations/0001_initial.py",
      "backend/api_usage/migrations/0002_enable_rls.py",
      "backend/api_usage/migrations/0003_rename_api_deprec_tenant_status_idx_api_depreca_tenant__60e9d0_idx_and_more.py",
      "backend/api_usage/migrations/0004_brin_indexes.py",
      "backend/api_usage/migrations/0005_merge_20251001_1316.py",
      "backend/api_usage/migrations/0006_standardize_rls_gucs.py",
      "backend/api_usage/migrations/__init__.py",
      "backend/audit/migrations/0001_initial.py",
      "backend/audit/migrations/0002_rls_and_brin.py",
      "backend/audit/migrations/0003_dedup_unique.py",
      "backend/audit/migrations/0003_partition_shadow_table.py",
      "backend/audit/migrations/0004_alter_auditeventv2_options_and_more.py",
      "backend/audit/migrations/0004_audit_export_job.py",
      "backend/audit/migrations/0005_audit_ingest_keys.py",
      "backend/audit/migrations/0005_audit_phase3_enhancements.py",
      "backend/audit/migrations/0006_auditpolicy_legal_hold.py",
      "backend/audit/migrations/0007_auditpolicy_retention_status.py",
      "backend/audit/migrations/0008_auditeventv2_perf_indexes.py",
      "backend/audit/migrations/0009_merge_0004_0008.py",
      "backend/audit/migrations/0010_drop_audit_event_legacy.py",
      "backend/audit/migrations/0011_alter_auditexportjob_id.py",
      "backend/audit/migrations/0012_auditexportjob_expires_at_auditexportjob_format_and_more.py",
      "backend/audit/migrations/0013_standardize_rls_gucs.py",
      "backend/audit/migrations/0014_actor_id_string.py",
      "backend/audit/migrations/FOLDER.migrations.md",
      "backend/audit/migrations/__init__.py",
      "backend/automations/migrations/0001_initial.py",
      "backend/automations/migrations/0002_definition.py",
      "backend/automations/migrations/0003_data_model_rest.py",
      "backend/automations/migrations/0004_run_logs.py",
      "backend/automations/migrations/0005_enable_rls.py",
      "backend/automations/migrations/0006_check_constraints.py",
      "backend/automations/migrations/0007_performance_indexes.py",
      "backend/automations/migrations/0008_brin_indexes.py",
      "backend/automations/migrations/0009_partial_indexes.py",
      "backend/automations/migrations/0010_remove_automationdefinition_auto_def_tenant_status_idx_and_more.py",
      "backend/automations/migrations/0011_merge_20251106_2056.py",
      "backend/automations/migrations/0012_remove_automationdefinition_created_by_and_more.py",
      "backend/automations/migrations/0013_standardize_rls_gucs.py",
      "backend/automations/migrations/0014_eventdeadletter_payload_json.py",
      "backend/automations/migrations/__init__.py",
      "backend/collaboration/migrations/0001_initial.py",
      "backend/collaboration/migrations/0002_review_models.py",
      "backend/collaboration/migrations/0003_rename_collaborati_locatio_idx_collaborati_locatio_8dcb41_idx_and_more.py",
      "backend/collaboration/migrations/__init__.py",
      "backend/community/migrations/0001_initial.py",
      "backend/community/migrations/0002_enable_rls_policies.py",
      "backend/community/migrations/0003_alter_implicitsignal_target_type.py",
      "backend/community/migrations/__init__.py",
      "backend/controls/migrations/0001_initial.py",
      "backend/controls/migrations/0002_performance_indexes.py",
      "backend/controls/migrations/0003_custom_dashboards.py",
      "backend/controls/migrations/0004_remove_controldefinition_ctrl_tenant_status_domain_idx_and_more.py",
      "backend/controls/migrations/0005_add_control_assessment_items.py",
      "backend/controls/migrations/0006_add_scope_dsl_fields.py",
      "backend/controls/migrations/0007_rename_ctrl_item_tenant_occ_idx_controls_co_tenant__9e7728_idx_and_more.py",
      "backend/controls/migrations/0008_access_review_fields.py",
      "backend/controls/migrations/0009_item_validity_fields.py",
      "backend/controls/migrations/0009_rename_controls_co_tenant_c_kind_idx_controls_co_tenant__9e629c_idx_and_more.py",
      "backend/controls/migrations/0010_merge_20251007_1220.py",
      "backend/controls/migrations/0010_occurrence_signoff_fields.py",
      "backend/controls/migrations/0011_merge_20251007_1252.py",
      "backend/controls/migrations/0012_rename_controls_occ_signoff_due_idx_controls_co_signoff_7bb034_idx.py",
      "backend/controls/migrations/0013_controldefinition_business_unit_and_more.py",
      "backend/controls/migrations/0014_add_composite_indexes.py",
      "backend/controls/migrations/0014_add_performance_indexes.py",
      "backend/controls/migrations/0015_merge_20251104_0914.py",
      "backend/controls/migrations/0016_evidence_and_more.py",
      "backend/controls/migrations/0017_add_search_vector_control.py",
      "backend/controls/migrations/0018_controldefinition_idx_control_search.py",
      "backend/controls/migrations/0019_controldefinition_idx_control_search.py",
      "backend/controls/migrations/0020_enable_rls.py",
      "backend/controls/migrations/0021_evidence_artifact_scan_fields.py",
      "backend/controls/migrations/0022_framework_requirement_and_policy_mapping.py",
      "backend/controls/migrations/0023_populate_framework_requirements.py",
      "backend/controls/migrations/0024_rename_controls_fr_framewo_idx_cat_controls_fr_framewo_83042f_idx_and_more.py",
      "backend/controls/migrations/__init__.py",
      "backend/controls/permissions.py",
      "backend/controls/tests/test_permissions.py",
      "backend/controls/tests/test_rbac_boundary.py",
      "backend/core/management/commands/create_search_permissions.py",
      "backend/core/migrations/0001_initial.py",
      "backend/core/migrations/0003_inapp_security_evidence.py",
      "backend/core/migrations/0004_alerts_evidence_meta_url.py",
      "backend/core/migrations/0005_auditevent_healthcheck_and_more.py",
      "backend/core/migrations/0006_outbound_email_job.py",
      "backend/core/migrations/0007_emailjob_partial_idx.py",
      "backend/core/migrations/0008_outbound_email_bodyhash_unique.py",
      "backend/core/migrations/0009_remove_outboundemailjob_core_emailjob_triplet_uniq_and_more.py",
      "backend/core/migrations/0010_pg_stat_statements_extension.py",
      "backend/core/migrations/0011_delete_auditevent.py",
      "backend/core/migrations/0012_drop_core_auditevent.py",
      "backend/core/migrations/0013_rlsauditevent.py",
      "backend/core/migrations/0014_designsystempage_designsystemcomponent_and_more.py",
      "backend/core/migrations/0015_add_planned_components.py",
      "backend/core/migrations/0016_add_resource_permission_models.py",
      "backend/core/migrations/0016_add_resource_permission_models.py",
      "backend/core/migrations/0017_rlsauditevent.py",
      "backend/core/migrations/0018_change_default_visibility_to_tenant.py",
      "backend/core/migrations/0019_tenantattribute_moduleattributeconfig_and_more.py",
      "backend/core/migrations/0020_queryperformancelog_queryperformancestats.py",
      "backend/core/migrations/0021_rename_core_queryp_created_af2bd6_idx_core_queryp_created_ff0917_idx_and_more.py",
      "backend/core/migrations/0022_merge_20251106_2056.py",
      "backend/core/migrations/0023_search_analytics_models.py",
      "backend/core/migrations/0024_alter_searchanalytics_created_by_and_more.py",
      "backend/core/migrations/0025_export_job.py",
      "backend/core/migrations/0026_enable_rls_core_export_job.py",
      "backend/core/migrations/0027_alter_exportjob_format_alter_exportjob_status.py",
      "backend/core/migrations/FOLDER.migrations.md",
      "backend/core/migrations/__init__.py",
      "backend/core/permissions.py",
      "backend/core/permissions/__init__.py",
      "backend/core/permissions/decorators.py",
      "backend/core/permissions/helpers.py",
      "backend/core/permissions/policy.py",
      "backend/core/permissions/rbac.py",
      "backend/core/permissions/rls_queryset_manager.py",
      "backend/core/permissions/test_utils.py",
      "backend/core/permissions/tests/__init__.py",
      "backend/core/permissions/tests/test_decorators.py",
      "backend/core/permissions/tests/test_helpers.py",
      "backend/core/permissions/tests/test_policy.py",
      "backend/core/tests/test_search/test_search_permissions.py",
      "backend/deployment-guide.md",
      "backend/directory/migrations/0001_initial.py",
      "backend/directory/migrations/0002_bitemporal_constraints.py",
      "backend/directory/migrations/0003_add_service_offering_technology.py",
      "backend/directory/migrations/0004_bitemporal_exclusion_constraints.py",
      "backend/directory/migrations/0005_check_constraints.py",
      "backend/directory/migrations/0006_alter_technologycomponent_unique_together_and_more.py",
      "backend/directory/migrations/0007_service_offering_technology_constraints_and_cleanup.py",
      "backend/directory/migrations/0008_legalentity_categories_serviceoffering_categories_and_more.py",
      "backend/directory/migrations/0009_legalentity_serviceoffering_expansion.py",
      "backend/directory/migrations/0010_legalentity_industry_sanctions_jurisdiction.py",
      "backend/directory/migrations/0011_allow_null_legal_entity_on_service_offering.py",
      "backend/directory/migrations/0012_remove_technologycomponent_categories_and_more.py",
      "backend/directory/migrations/0013_fix_techcat_null_distinct.py",
      "backend/directory/migrations/0014_remove_serviceoffering_tags.py",
      "backend/directory/migrations/0015_technologyproduct_categories.py",
      "backend/directory/migrations/0016_technologycategory_is_active.py",
      "backend/directory/migrations/__init__.py",
      "backend/documents/deployment-guide.md",
      "backend/documents/migrations/0001_initial.py",
      "backend/documents/migrations/0002_rename_doc_t_type_deleted_idx_documents_d_tenant__5ef7dd_idx_and_more.py",
      "backend/documents/migrations/0003_enable_rls.py",
      "backend/documents/migrations/0004_expand_doctype_and_relations.py",
      "backend/documents/migrations/0005_documentslot_and_status.py",
      "backend/documents/migrations/0006_documenttypeprofile.py",
      "backend/documents/migrations/__init__.py",
      "backend/environment/migrations/0001_initial.py",
      "backend/environment/migrations/0002_add_owned_resource_mixin.py",
      "backend/environment/migrations/0002_initial.py",
      "backend/environment/migrations/0002_riskrule_riskrulefielddefinition_riskruleexecution.py",
      "backend/environment/migrations/0003_add_business_security_ownership.py",
      "backend/environment/migrations/0004_alter_asset_visibility_and_more.py",
      "backend/environment/migrations/0005_remove_asset_criticality_asset_service_asset_tier.py",
      "backend/environment/migrations/0006_add_composite_indexes.py",
      "backend/environment/migrations/0006_add_performance_indexes.py",
      "backend/environment/migrations/0007_merge_20251104_0914.py",
      "backend/environment/migrations/0008_remove_asset_env_asset_lifecycle_risk_idx_and_more.py",
      "backend/environment/migrations/0009_merge_20251106_2056.py",
      "backend/environment/migrations/0010_remove_asset_environment_owner_t_925258_idx_and_more.py",
      "backend/environment/migrations/0011_asset_idx_asset_type_tier_stat_and_more.py",
      "backend/environment/migrations/0012_add_search_vector_asset.py",
      "backend/environment/migrations/0013_asset_idx_asset_search.py",
      "backend/environment/migrations/0014_asset_managed_by_thirdpartyentity.py",
      "backend/environment/migrations/0015_alter_asset_unique_together_and_more.py",
      "backend/environment/migrations/0017_bitemporal_table_maintenance_tuning.py",
      "backend/environment/migrations/0018_asset_constraints_and_assettechnology_pair_unique.py",
      "backend/environment/migrations/0019_enable_rls.py",
      "backend/environment/migrations/0020_standardize_risk_fields.py",
      "backend/environment/migrations/0021_merge_20251216_1225.py",
      "backend/environment/migrations/0022_rename_env_riskrule_tenant_target_idx_environment_tenant__8ca752_idx_and_more.py",
      "backend/environment/migrations/0023_riskrulefielddefinition_category.py",
      "backend/environment/migrations/0024_add_asset_risk_breakdown.py",
      "backend/environment/migrations/0025_sprint14_asset_enhancements.py",
      "backend/environment/migrations/0026_rename_env_compmap_t_stat_idx_environment_tenant__24f617_idx_and_more.py",
      "backend/environment/migrations/0026_update_asset_search_vector_business_unit_option.py",
      "backend/environment/migrations/0027_merge_20251224_0905.py",
      "backend/environment/migrations/0028_asset_hosting_model_asset_local_service_and_more.py",
      "backend/environment/migrations/0029_asset_data_model_v12_1.py",
      "backend/environment/migrations/0030_remove_asset_idx_asset_category_and_more.py",
      "backend/environment/migrations/0031_risk_rule_library.py",
      "backend/environment/migrations/0032_remove_orgrulevisibility_unique_org_rule_visibility_and_more.py",
      "backend/environment/migrations/0033_add_asset_domain_registration_fields.py",
      "backend/environment/migrations/0034_alter_asset_asset_type.py",
      "backend/environment/migrations/0035_domain_analyzer_integration_sprint16.py",
      "backend/environment/migrations/0036_asset_discovery_tracking_fields.py",
      "backend/environment/migrations/0036_rename_idx_certhistory_asset_environment_tenant__3daa83_idx_and_more.py",
      "backend/environment/migrations/0037_merge_20260109_0700.py",
      "backend/environment/migrations/0038_threat_intelligence_traffic_ranking.py",
      "backend/environment/migrations/0039_remove_asset_idx_asset_threat_malicious_and_more.py",
      "backend/environment/migrations/0040_technology_fingerprinting.py",
      "backend/environment/migrations/0041_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0042_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0043_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0044_asset_discovery_sources.py",
      "backend/environment/migrations/0045_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0046_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "backend/environment/migrations/0047_update_threatintelligencecheck_constraint.py",
      "backend/environment/migrations/0048_backfill_asset_discovery_sources.py",
      "backend/environment/migrations/0049_asset_directory_category.py",
      "backend/environment/migrations/0050_remove_technologycategory_parent_and_more.py",
      "backend/environment/migrations/0051_remove_asset_business_function_id_and_more.py",
      "backend/environment/migrations/__init__.py",
      "backend/environment/permissions.py",
      "backend/environment/risk_rule_library_permissions.py",
      "backend/environment/risk_rule_permissions.py",
      "backend/events/migrations/0001_initial.py",
      "backend/events/migrations/0002_add_owned_resource_mixin.py",
      "backend/events/migrations/0003_add_business_security_ownership.py",
      "backend/events/migrations/0004_alter_event_visibility_and_more.py",
      "backend/events/migrations/0005_incident.py",
      "backend/events/migrations/0006_delete_incident.py",
      "backend/events/migrations/0007_incident_assetentity.py",
      "backend/events/migrations/0008_alter_incident_created_at_alter_incident_created_by_and_more.py",
      "backend/events/migrations/0009_alter_incident_business_owner_team_and_more.py",
      "backend/events/migrations/__init__.py",
      "backend/frameworks/migrations/__init__.py",
      "backend/gcp/deploy.sh",
      "backend/gcp/setup-search-infrastructure.sh",
      "backend/guide-migrations.md",
      "backend/information/migrations/0001_initial.py",
      "backend/information/migrations/0001_initial_ims_models.py",
      "backend/information/migrations/0002_alter_document_content_type_and_more.py",
      "backend/information/migrations/0003_merge_20251106_2056.py",
      "backend/information/migrations/0004_assetentity_fks_and_contenttypes.py",
      "backend/information/migrations/0005_alter_privacyprofile_content_type.py",
      "backend/information/migrations/__init__.py",
      "backend/information/models/migration.py",
      "backend/information/serializers/migration.py",
      "backend/integrations/migrations/0001_initial.py",
      "backend/integrations/migrations/0002_alter_integrationconnection_provider.py",
      "backend/integrations/migrations/0003_slackinstall.py",
      "backend/integrations/migrations/0004_rename_integrations_slack_tenant_team_idx_integration_tenant__f6ace6_idx.py",
      "backend/integrations/migrations/0005_enhance_integrationconnection_for_adapter_pattern.py",
      "backend/integrations/migrations/0006_enhance_integration_connection.py",
      "backend/integrations/migrations/0007_integrationfieldmapping.py",
      "backend/integrations/migrations/0008_scaling_architecture.py",
      "backend/integrations/migrations/0009_alter_integrationprovider_options_and_more.py",
      "backend/integrations/migrations/0010_integrationaction_integrationdatapoint.py",
      "backend/integrations/migrations/0011_seed_google_workspace_actions_complete.py",
      "backend/integrations/migrations/0012_integrationaction_category.py",
      "backend/integrations/migrations/0013_seed_google_workspace_webhooks.py",
      "backend/integrations/migrations/0014_seed_slack_provider_and_actions.py",
      "backend/integrations/migrations/0015_seed_github_provider_and_actions.py",
      "backend/integrations/migrations/0016_add_is_automation_enabled.py",
      "backend/integrations/migrations/0017_integrationaction_integration_auto_en_idx_and_more.py",
      "backend/integrations/migrations/0018_integration_sync_history.py",
      "backend/integrations/migrations/0019_add_sync_history_data_snapshots.py",
      "backend/integrations/migrations/0020_add_sync_type_choices.py",
      "backend/integrations/migrations/0021_rename_nango_connection_id.py",
      "backend/integrations/migrations/0022_normalize_integration_provider_categories.py",
      "backend/integrations/migrations/0023_seed_microsoft_365_provider.py",
      "backend/integrations/migrations/0024_seed_microsoft_teams_provider.py",
      "backend/integrations/migrations/0025_normalize_connected_status_to_active.py",
      "backend/integrations/migrations/0026_seed_google_workspace_provider.py",
      "backend/integrations/migrations/__init__.py",
      "backend/integrations/tests/test_token_lifecycle_guardrails.py",
      "backend/integrations/tests/test_token_refresh.py",
      "backend/integrations/token-lifecycle-standard.md",
      "backend/knowledge/migrations/0001_initial.py",
      "backend/knowledge/migrations/0002_remove_controlmapping_unique_policy_requirement_and_more.py",
      "backend/knowledge/migrations/__init__.py",
      "backend/localization/migrations/0001_initial.py",
      "backend/localization/migrations/0002_add_owned_resource_mixin.py",
      "backend/localization/migrations/0003_add_business_security_ownership.py",
      "backend/localization/migrations/0004_alter_glossaryterm_visibility_and_more.py",
      "backend/localization/migrations/0005_add_analytics_models.py",
      "backend/localization/migrations/0006_alter_translationchangelog_created_by_and_more.py",
      "backend/localization/migrations/0007_translation_ai_config.py",
      "backend/localization/migrations/__init__.py",
      "backend/manual-deploy-with-verify.sh",
      "backend/mapping_intelligence/migrations/0001_initial.py",
      "backend/mapping_intelligence/migrations/0002_add_missing_fields.py",
      "backend/mapping_intelligence/migrations/0002_fielddefinition_mapping_int_synonym_49b140_gin.py",
      "backend/mapping_intelligence/migrations/0003_add_aimachinesettings.py",
      "backend/mapping_intelligence/migrations/0003_fielddefinition_tenant_scope.py",
      "backend/mapping_intelligence/migrations/0003_rename_mapping_int_entity__idx_mapping_int_entity__9ab5a0_idx_and_more.py",
      "backend/mapping_intelligence/migrations/0004_merge_20251020_1449.py",
      "backend/mapping_intelligence/migrations/0005_add_versioning_and_constraints.py",
      "backend/mapping_intelligence/migrations/0007_add_performance_indexes.py",
      "backend/mapping_intelligence/migrations/0008_merge_20251106_2056.py",
      "backend/mapping_intelligence/migrations/0009_mappinghistory_updated_at_mappinghistory_updated_by_and_more.py",
      "backend/mapping_intelligence/migrations/0010_merge_20260105_1105.py",
      "backend/mapping_intelligence/migrations/0011_remove_fielddefinition_mapping_int_entity__030d87_idx_and_more.py",
      "backend/mapping_intelligence/migrations/__init__.py",
      "backend/mapping_intelligence/permissions.py",
      "backend/menu_overrides/migrations/0001_initial.py",
      "backend/menu_overrides/migrations/0002_add_navigation_analytics.py",
      "backend/menu_overrides/migrations/__init__.py",
      "backend/middleware/rbac_enforcement.py",
      "backend/onboarding/migrations/0001_initial.py",
      "backend/onboarding/migrations/0002_onboardingruntimestate.py",
      "backend/onboarding/migrations/__init__.py",
      "backend/operational/migrations/0001_initial.py",
      "backend/operational/migrations/0002_event_sourcing_triggers.py",
      "backend/operational/migrations/0003_fix_event_sourcing_trigger.py",
      "backend/operational/migrations/0004_trigger_request_id.py",
      "backend/operational/migrations/0005_trigger_request_id_metadata.py",
      "backend/operational/migrations/0006_trigger_update_merge_guard.py",
      "backend/operational/migrations/0007_trigger_merge_guard_jsonb.py",
      "backend/operational/migrations/__init__.py",
      "backend/ops/scripts/deploy-celery-jobs.sh",
      "backend/ops/scripts/deploy-rbac-seed.sh",
      "backend/ops/scripts/deploy-rbac-seed.sh",
      "backend/ops/scripts/execute-rbac-seed.sh",
      "backend/ops/scripts/run-migrations.sh",
      "backend/ops/scripts/seed-rbac-permissions.sh",
      "backend/page_actions/migrations/0001_initial.py",
      "backend/page_actions/migrations/0002_remove_customaction_unique_custom_action_per_org_page_and_more.py",
      "backend/page_actions/migrations/0003_standardize_rls_gucs.py",
      "backend/page_actions/migrations/__init__.py",
      "backend/page_actions/permissions.py",
      "backend/page_actions/services/permission_service.py",
      "backend/page_actions/tests/test_permission_service.py",
      "backend/posture/finding_template_library_permissions.py",
      "backend/posture/migrations/0001_initial.py",
      "backend/posture/migrations/0002_add_owned_resource_mixin.py",
      "backend/posture/migrations/0003_add_business_security_ownership.py",
      "backend/posture/migrations/0004_alter_campaign_visibility_alter_finding_visibility_and_more.py",
      "backend/posture/migrations/0005_add_search_vector_finding.py",
      "backend/posture/migrations/0006_finding_idx_finding_search.py",
      "backend/posture/migrations/0007_alter_campaign_scope_assets_alter_finding_asset_and_more.py",
      "backend/posture/migrations/0008_add_finding_likelihood_and_targets.py",
      "backend/posture/migrations/0009_add_finding_template_model.py",
      "backend/posture/migrations/0010_finding_template_library.py",
      "backend/posture/migrations/0011_seed_finding_template_library.py",
      "backend/posture/migrations/0012_rename_posture_ftl_category_status_idx_posture_ftl_cat_status_idx_and_more.py",
      "backend/posture/migrations/__init__.py",
      "backend/run_migrations.py",
      "backend/scripts/audit_rbac_migration.py",
      "backend/scripts/audit_rbac_migration.py",
      "backend/scripts/debug/test_automations_permission_debug.py",
      "backend/scripts/debug/test_rbac_migration.py",
      "backend/scripts/debug/test_rbac_migration.py",
      "backend/setup-auto-deploy.sh",
      "backend/tasks/migrations/0001_initial.py",
      "backend/tasks/migrations/0002_tasklink.py",
      "backend/tasks/migrations/0003_tasksavedview.py",
      "backend/tasks/migrations/0004_task_tags.py",
      "backend/tasks/migrations/0005_task_comments_watchers.py",
      "backend/tasks/migrations/0006_task_attachment.py",
      "backend/tasks/migrations/0007_checklist_item.py",
      "backend/tasks/migrations/0008_alter_checklistitem_created_at_and_more.py",
      "backend/tasks/migrations/0008_task_workflow_sla.py",
      "backend/tasks/migrations/0009_task_provenance.py",
      "backend/tasks/migrations/0010_merge_20251006_1220.py",
      "backend/tasks/migrations/0011_add_owned_resource_mixin.py",
      "backend/tasks/migrations/0016_task_completion_rule.py",
      "backend/tasks/migrations/0017_add_business_security_ownership.py",
      "backend/tasks/migrations/0018_alter_checklistitem_visibility_alter_task_visibility_and_more.py",
      "backend/tasks/migrations/0019_add_search_vector_task.py",
      "backend/tasks/migrations/0020_task_idx_task_search.py",
      "backend/tasks/migrations/0021_enable_rls.py",
      "backend/tasks/migrations/0022_add_task_decisions.py",
      "backend/tasks/migrations/0023_convert_task_decision_to_task_type.py",
      "backend/tasks/migrations/0024_enforce_single_pending_task_decision.py",
      "backend/tasks/migrations/0025_alter_task_status_alter_task_type.py",
      "backend/tasks/migrations/__init__.py",
      "backend/tests/admin/test_admin_notifications_policy_rbac.py",
      "backend/tests/admin/test_admin_permission_audit.py",
      "backend/tests/admin/test_admin_permission_audit_correlation.py",
      "backend/tests/admin/test_admin_roles_rbac_edit_allow.py",
      "backend/tests/admin/test_admin_roles_rbac_edit_deny.py",
      "backend/tests/admin/test_admin_users_rbac_allow.py",
      "backend/tests/audit/test_audit_events_rbac_deny_audit.py",
      "backend/tests/audit/test_audit_export_rbac_deny.py",
      "backend/tests/audit/test_audit_export_rbac_superuser.py",
      "backend/tests/audit/test_audit_rbac.py",
      "backend/tests/audit/test_audit_rbac_endpoints.py",
      "backend/tests/audit/test_audit_registry_billing_vendor_required.py",
      "backend/tests/audit/test_audit_registry_permissions_required.py",
      "backend/tests/critical/test_auth_oidc.py",
      "backend/tests/integration/test_auth0_idp_asset_bootstrap.py",
      "backend/tests/integration/test_collaboration_authorization.py",
      "backend/tests/integration/test_db_viewer_rbac_allow.py",
      "backend/tests/integration/test_schema_permissions.py",
      "backend/tests/integration/test_schema_ui_permissions.py",
      "backend/tests/integration/test_secret_hashing.py",
      "backend/tests/integration/test_teams_rbac.py",
      "backend/tests/integration/test_teams_rbac_allow.py",
      "backend/tests/integration/test_thirdparty_authorization.py",
      "backend/tests/integration/test_thirdparty_relationship_authorization.py",
      "backend/tests/security/test_auth_flows_comprehensive.py",
      "backend/tests/security/test_auth_login_ratelimit.py",
      "backend/tests/security/test_auth_logout_csrf.py",
      "backend/tests/security/test_auth_session.py",
      "backend/tests/security/test_impersonation_rbac.py",
      "backend/tests/security/test_rbac_admin_api.py",
      "backend/tests/security/test_rbac_casl_mapping.py",
      "backend/tests/security/test_rbac_forbidden_json.py",
      "backend/tests/security/test_rbac_forbidden_json_shape.py",
      "backend/tests/security/test_rbac_risk_matrix.py",
      "backend/tests/security/test_rbac_risk_recalc_command.py",
      "backend/tests/security/test_rbac_settings_guard.py",
      "backend/tests/security/test_thirdparty_unauth_endpoints.py",
      "backend/tests/suppliers/test_suppliers_reports_rbac.py",
      "backend/thirdparties/migrations/0001_initial.py",
      "backend/thirdparties/migrations/0002_enable_rls_policies.py",
      "backend/thirdparties/migrations/0003_bitemporal_constraints.py",
      "backend/thirdparties/migrations/0004_rename_tables_suppliers_to_thirdparties.py",
      "backend/thirdparties/migrations/0005_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
      "backend/thirdparties/migrations/0006_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
      "backend/thirdparties/migrations/0007_add_asset_service_offering.py",
      "backend/thirdparties/migrations/0008_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
      "backend/thirdparties/migrations/0009_supplier_graph_view.py",
      "backend/thirdparties/migrations/0010_add_missing_fields.py",
      "backend/thirdparties/migrations/0011_enable_rls.py",
      "backend/thirdparties/migrations/0012_remove_thirdpartyrelationship_thirdparty_rel_unique_and_more.py",
      "backend/thirdparties/migrations/0012_suppliers_saved_view.py",
      "backend/thirdparties/migrations/0013_asset_service_offering_constraints.py",
      "backend/thirdparties/migrations/0014_check_constraints.py",
      "backend/thirdparties/migrations/0015_performance_indexes.py",
      "backend/thirdparties/migrations/0016_partial_indexes.py",
      "backend/thirdparties/migrations/0017_add_frontend_aligned_fields.py",
      "backend/thirdparties/migrations/0018_add_gdpr_data_privacy_models.py",
      "backend/thirdparties/migrations/0019_remove_dataprivacycontact_created_by_and_more.py",
      "backend/thirdparties/migrations/0020_add_privacy_enhancements.py",
      "backend/thirdparties/migrations/0021_remove_dataprivacycontact_created_by_and_more.py",
      "backend/thirdparties/migrations/0022_repair_privacy_columns.py",
      "backend/thirdparties/migrations/0023_remove_asset_asset_tenant_type_idx_and_more.py",
      "backend/thirdparties/migrations/0024_remove_asset_asset_tenant_type_idx_and_more.py",
      "backend/thirdparties/migrations/0025_alter_document_content_type_and_more.py",
      "backend/thirdparties/migrations/0026_supplierassessment_supplierchangerequest_and_more.py",
      "backend/thirdparties/migrations/0027_dataprivacycontact_dataprivacyprofile_and_more.py",
      "backend/thirdparties/migrations/0028_merge_20251023_1923.py",
      "backend/thirdparties/migrations/0029_tprm_policy_owner_team_doc_source.py",
      "backend/thirdparties/migrations/0030_rename_tp_tenant_owner_user_idx_thirdpartie_tenant__4ef8e4_idx_and_more.py",
      "backend/thirdparties/migrations/0031_add_business_security_ownership.py",
      "backend/thirdparties/migrations/0032_dataprivacycontact_dataprivacyprofile_and_more.py",
      "backend/thirdparties/migrations/0033_alter_thirdparty_visibility.py",
      "backend/thirdparties/migrations/0034_alter_thirdparty_relationship_types.py",
      "backend/thirdparties/migrations/0035_thirdparty_frameworks_alter_thirdparty_tags.py",
      "backend/thirdparties/migrations/0036_alter_thirdparty_tags.py",
      "backend/thirdparties/migrations/0037_add_composite_indexes.py",
      "backend/thirdparties/migrations/0037_add_performance_indexes.py",
      "backend/thirdparties/migrations/0038_merge_20251104_0914.py",
      "backend/thirdparties/migrations/0039_remove_directorylinkconfig_tp_link_sync_idx_and_more.py",
      "backend/thirdparties/migrations/0041_search_extensions_and_indexes.py",
      "backend/thirdparties/migrations/0042_add_search_vector_thirdparty.py",
      "backend/thirdparties/migrations/0043_thirdparty_idx_thirdparty_search.py",
      "backend/thirdparties/migrations/0044_thirdparty_entity_versioning.py",
      "backend/thirdparties/migrations/0045_dataprivacyprofile_third_party_entity.py",
      "backend/thirdparties/migrations/0046_rename_thirdparty_tenant_entity_idx_thirdpartie_tenant__205199_idx_and_more.py",
      "backend/thirdparties/migrations/0047_standardize_rls_gucs.py",
      "backend/thirdparties/migrations/0048_alter_directorylinkconfig_linked_legal_entity_and_more.py",
      "backend/thirdparties/migrations/0049_fix_thirdparty_no_overlap_valid_to_infinity.py",
      "backend/thirdparties/migrations/0050_bitemporal_table_maintenance_tuning.py",
      "backend/thirdparties/migrations/0051_standardize_risk_fields.py",
      "backend/thirdparties/migrations/0052_alter_thirdparty_risk_factors_and_more.py",
      "backend/thirdparties/migrations/0053_directorylinkconfig_linked_local_provider.py",
      "backend/thirdparties/migrations/0054_alter_thirdparty_lifecycle_status.py",
      "backend/thirdparties/migrations/0055_thirdparty_adn_parity_fields.py",
      "backend/thirdparties/migrations/0056_functionalrole_industrycodecrosswalk_and_more.py",
      "backend/thirdparties/migrations/0057_seed_functional_roles.py",
      "backend/thirdparties/migrations/0058_seed_industry_crosswalk.py",
      "backend/thirdparties/migrations/0059_thirdparty_adn_parity_fields.py",
      "backend/thirdparties/migrations/0060_supplier_directory_category.py",
      "backend/thirdparties/migrations/0061_thirdparty_control_frameworks.py",
      "backend/thirdparties/migrations/0062_migrate_frameworks_m2m.py",
      "backend/thirdparties/migrations/0063_alter_thirdparty_frameworks.py",
      "backend/thirdparties/migrations/FOLDER.migrations.md",
      "backend/thirdparties/migrations/__init__.py",
      "backend/webhooks/migrations/0001_initial.py",
      "backend/webhooks/migrations/0002_rename_webhooks_de_subscri_f5d8c1_idx_webhooks_de_subscri_f97236_idx_and_more.py",
      "backend/webhooks/migrations/0003_unique_constraints.py",
      "backend/webhooks/migrations/0004_add_owned_resource_mixin.py",
      "backend/webhooks/migrations/0005_add_business_security_ownership.py",
      "backend/webhooks/migrations/0006_alter_webhookdelivery_visibility_and_more.py",
      "backend/webhooks/migrations/0007_hash_webhook_secrets.py",
      "backend/webhooks/migrations/0007_hash_webhook_secrets.py",
      "backend/webhooks/migrations/0008_alter_webhooksubscription_secret.py",
      "backend/webhooks/migrations/0008_alter_webhooksubscription_secret.py",
      "backend/webhooks/migrations/__init__.py",
      "contracts/migration-to-schemathesis.md",
      "devops/grafana/dashboards/rbac-dashboard.json",
      "devops/prometheus/rules/rbac-alerts.yml",
      "docs/access-auth.md",
      "docs/agents/context/admin.billing.md",
      "docs/api/rbac-api-reference.md",
      "docs/api/rbac-api.md",
      "docs/api/rbac-openapi.json",
      "docs/api/rbac-quick-reference.md",
      "docs/architecture/architecture-deployment.md",
      "docs/architecture/rbac-architecture.md",
      "docs/architecture/security-audit-rbac.md",
      "docs/design-system/automated-deployment-setup.md",
      "docs/design-system/design-tokens-tier-guide.md",
      "docs/feature-flags/catalog-key-migration.md",
      "docs/feature-specs/admin/page-actions/09-deployment-guide.md",
      "docs/feature-specs/controls/deployment-checklist.md",
      "docs/feature-specs/information/my-environment-rbac-integration.md",
      "docs/feature-specs/rbac/admin-roles-review-and-cleanup.md",
      "docs/feature-specs/rbac/rbac-spec.md",
      "docs/feature-specs/search-deployment-guide.md",
      "docs/guides/authentication-setup.md",
      "docs/guides/cost-optimized-deployment.md",
      "docs/guides/deployment-guide-permissions.md",
      "docs/guides/deployment-guide-permissions.md",
      "docs/guides/multi-tenant-deployment-critical.md",
      "docs/guides/post-deployment-setup.md",
      "docs/guides/post-deployment-verification.md",
      "docs/guides/rbac-admin-guide.md",
      "docs/permissions.md",
      "docs/plans/infrastructure-options-comparison.md",
      "docs/prd/rbac-simplified-design.md",
      "docs/rbac-cache-implementation.md",
      "docs/reference/rbac.yaml",
      "docs/reference/reference-rbac-permission-sync.md",
      "docs/runbooks/deploy-admin.md",
      "docs/runbooks/deployment-checklist.md",
      "docs/runbooks/rbac-operations-runbook.md",
      "docs/runbooks/rbac-risk-policy.md",
      "docs/runbooks/runbook-deployment-best-practices.md",
      "docs/runbooks/runbook-production-deployment.md",
      "docs/secret-management-plan.md",
      "docs/security/rbac-risk-policy.md",
      "e2e/fixtures/auth.fixture.ts",
      "e2e/page-objects/auth/login.page.ts",
      "e2e/tests/auth/authentication.spec.ts",
      "manual-deployment-steps.md",
      "migration-complete.md",
      "migration-status.md",
      "migrations-applied-success.md",
      "packages/app-shared/src/app/AuthenticatedApp.tsx",
      "packages/app-shared/src/auth/AbilityContext.shared.ts",
      "packages/app-shared/src/auth/AbilityProvider.ts",
      "packages/app-shared/src/auth/AbilityProviderRoot.tsx",
      "packages/app-shared/src/auth/AuthError.tsx",
      "packages/app-shared/src/auth/FOLDER.auth.md",
      "packages/app-shared/src/auth/NoTenantAccess.tsx",
      "packages/app-shared/src/auth/SessionExpiryWarningProvider.tsx",
      "packages/app-shared/src/auth/SessionGate.tsx",
      "packages/app-shared/src/auth/UseEnvironmentPermissions.tsx",
      "packages/app-shared/src/auth/UseEnvironmentPermissions.tsx",
      "packages/app-shared/src/auth/__tests__/FOLDER.__tests__.md",
      "packages/app-shared/src/auth/__tests__/permissionGrouping.test.ts",
      "packages/app-shared/src/auth/__tests__/permissionGrouping.test.ts",
      "packages/app-shared/src/auth/ability.ts",
      "packages/app-shared/src/auth/can.ts",
      "packages/app-shared/src/auth/logoutBroadcast.ts",
      "packages/app-shared/src/auth/logoutClient.ts",
      "packages/app-shared/src/auth/permissionGrouping.ts",
      "packages/app-shared/src/auth/permissionGrouping.ts",
      "packages/app-shared/src/auth/rbac-canonical.ts",
      "packages/app-shared/src/auth/rbac-canonical.ts",
      "packages/app-shared/src/auth/session.ts",
      "packages/app-shared/src/auth/sessionExpiryWarningContext.ts",
      "packages/app-shared/src/auth/useSessionHeartbeat.ts",
      "packages/app-shared/src/components/admin/AdminBillingView.tsx",
      "packages/app-shared/src/components/admin/OrgBillingOverviewView.tsx",
      "packages/app-shared/src/components/admin/TenantBillingTab.tsx",
      "packages/app-shared/src/components/admin/roles/BatchPermissionUpdates.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionConflictDetector.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionMatrix.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionMatrixSkeleton.tsx",
      "packages/app-shared/src/components/admin/roles/PermissionsList.tsx",
      "packages/app-shared/src/components/admin/roles/__tests__/BatchPermissionUpdates.test.tsx",
      "packages/app-shared/src/components/auth/PermissionDenied.tsx",
      "packages/app-shared/src/components/auth/PermissionDenied.tsx",
      "packages/app-shared/src/constants/rbac-module-settings.md",
      "packages/app-shared/src/constants/rbac.ts",
      "packages/app-shared/src/features/admin/components/AdminBillingView.tsx",
      "packages/app-shared/src/features/admin/components/roles/PermissionConflictDetector.tsx",
      "packages/app-shared/src/features/admin/components/roles/PermissionMatrix.tsx",
      "packages/app-shared/src/features/admin/components/roles/PermissionMatrixSkeleton.tsx",
      "packages/app-shared/src/features/admin/components/roles/permissionConflictRules.ts",
      "packages/app-shared/src/features/admin/components/roles/permissionMatrix.shared.ts",
      "packages/app-shared/src/features/admin/hooks/useUsersAndPermissions.ts",
      "packages/app-shared/src/features/admin/pages/AdminBillingPage.tsx",
      "packages/app-shared/src/features/admin/pages/AdminMigrationDashboardPage.tsx",
      "packages/app-shared/src/features/admin/pages/AdminMigrationDashboardPageView.tsx",
      "packages/app-shared/src/features/auth/components/PermissionDenied.tsx",
      "packages/app-shared/src/features/auth/components/PermissionDenied.tsx",
      "packages/app-shared/src/features/auth/components/index.ts",
      "packages/app-shared/src/features/auth/index.ts",
      "packages/app-shared/src/features/auth/utils/ability.ts",
      "packages/app-shared/src/features/auth/utils/can.ts",
      "packages/app-shared/src/features/auth/utils/index.ts",
      "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
      "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
      "packages/app-shared/src/features/auth/utils/session.ts",
      "packages/app-shared/src/features/information/hooks/useMigration.ts",
      "packages/app-shared/src/features/information/pages/MigrationConflictsPage.tsx",
      "packages/app-shared/src/features/information/pages/MigrationDashboardPage.tsx",
      "packages/app-shared/src/features/information/pages/MigrationImportPage.tsx",
      "packages/app-shared/src/features/org/pages/OrgBillingPage.tsx",
      "packages/app-shared/src/hooks/admin/usePermissionsCatalog.ts",
      "packages/app-shared/src/hooks/admin/useUsersAndPermissions.ts",
      "packages/app-shared/src/hooks/information/useMigration.ts",
      "packages/app-shared/src/hooks/lib/parsePermissions.ts",
      "packages/app-shared/src/hooks/permissions/__tests__/useCanAccess.test.tsx",
      "packages/app-shared/src/hooks/permissions/__tests__/usePermission.test.tsx",
      "packages/app-shared/src/hooks/permissions/index.ts",
      "packages/app-shared/src/hooks/permissions/testUtils.tsx",
      "packages/app-shared/src/hooks/permissions/useAbility.ts",
      "packages/app-shared/src/hooks/permissions/useCanAccess.ts",
      "packages/app-shared/src/hooks/permissions/usePermission.ts",
      "packages/app-shared/src/hooks/useOrgBillingApi.ts",
      "packages/app-shared/src/lib/__tests__/permissions.test.ts",
      "packages/app-shared/src/lib/permissions.ts",
      "packages/app-shared/src/lib/personalTokensApi.ts",
      "packages/app-shared/src/pages/AuthLogoutPage.tsx",
      "packages/app-shared/src/pages/platform/AuthAnalyticsPage.impl.tsx",
      "packages/app-shared/src/pages/platform/AuthAnalyticsPage.tsx",
      "packages/app-shared/src/preauth/debug.ts",
      "packages/app-shared/src/preauth/index.ts",
      "packages/app-shared/src/preauth/network.ts",
      "packages/app-shared/src/preauth/session.ts",
      "packages/app-shared/src/preauth/telemetry.ts",
      "packages/app-shared/src/preauth/theme.ts",
      "packages/app-shared/src/preauth/types.ts",
      "packages/app-shared/src/preauth/ui.ts",
      "packages/app-shared/src/preauth/utils.test.ts",
      "packages/app-shared/src/preauth/utils.ts",
      "packages/app-shared/src/router/Unauthorized.tsx",
      "packages/app-shared/src/tests/admin.billing.a11y.test.tsx",
      "packages/app-shared/src/tests/admin.billing.exportmenu.test.tsx",
      "packages/app-shared/src/tests/admin.billing.mobile.test.tsx",
      "packages/app-shared/src/tests/admin.billing.toolbar.smoke.test.tsx",
      "packages/app-shared/src/tests/admin.users.rbac.banner.test.tsx",
      "packages/app-shared/src/tests/api.credentials.test.ts",
      "packages/app-shared/src/tests/auth.can.test.ts",
      "packages/app-shared/src/tests/permission.gate.test.tsx",
      "packages/app-shared/src/tests/router.unauthorized.ui.test.tsx",
      "packages/app-shared/src/tests/suppliers.directory.views.rbac.test.tsx",
      "packages/app-shared/src/types/rbac.ts",
      "packages/auth/package.json",
      "packages/auth/src/__tests__/permissionGrouping.test.ts",
      "packages/auth/src/__tests__/permissionGrouping.test.ts",
      "packages/auth/src/__tests__/rbac-canonical.test.ts",
      "packages/auth/src/__tests__/rbac-canonical.test.ts",
      "packages/auth/src/ability.ts",
      "packages/auth/src/can.ts",
      "packages/auth/src/index.ts",
      "packages/auth/src/logout/broadcast.ts",
      "packages/auth/src/logout/client.ts",
      "packages/auth/src/logout/index.ts",
      "packages/auth/src/permissionGrouping.ts",
      "packages/auth/src/permissionGrouping.ts",
      "packages/auth/src/rbac-canonical.ts",
      "packages/auth/src/rbac-canonical.ts",
      "packages/auth/src/session.ts",
      "packages/auth/test-results/junit.xml",
      "packages/auth/tsconfig.json",
      "packages/auth/tsconfig.tsbuildinfo",
      "packages/documentation/migration/page-checklist.json",
      "packages/types/src/rbac.ts",
      "packages/ui/.ai/design-tokens.json",
      "packages/ui/.ai/migration-rules.json",
      "packages/ui/src/components/molecules/TokenPicker/TokenPicker.tsx",
      "packages/ui/src/components/molecules/TokenPicker/index.ts",
      "packages/ui/src/tokens/components.css",
      "packages/ui/src/tokens/index.css",
      "packages/ui/src/tokens/index.ts",
      "packages/ui/src/tokens/primitives.css",
      "packages/ui/src/tokens/semantic.css",
      "packages/ui/src/tokens/themes/dark.css",
      "postgres-18-migration-guide.md",
      "rbac-cache-delivery.md",
      "rbac-cache-quickstart.md",
      "scripts/checks/check-customer-preauth-no-design-system.mjs",
      "scripts/ci/check-endpoint-permissions.mjs",
      "scripts/ci/check-permission-metadata.mjs",
      "scripts/ci/check-route-permissions.sh",
      "scripts/ci/check_migrations.sh",
      "scripts/ci/validate-rbac-sync.mjs",
      "scripts/deploy-types.cjs",
      "scripts/deployment/build-production.sh",
      "scripts/design-system/generate-token-json.mjs",
      "scripts/migration/audit-page-components.mjs",
      "scripts/validate-deployment.sh",
      "scripts/validation/validate_permissions.py",
      "scripts/verify-phase0-deployment.sh",
      "scripts/verify_migration.sh",
      "tests/contract/consumers/auth.contract.test.ts",
      "tools/mcp-mordor/src/tools/rbac.ts"
    ]
  },
  "commands": [
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli repo inspect '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' --json-output",
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph overview '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' --json-output",
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph expand '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' <anchor-id> --json-output"
  ]
}
```

## Aethyme Pack

```json
{
  "task": {
    "raw": "Explain this repo",
    "normalized": "explain this repo",
    "kind": "explain_repo"
  },
  "summary": {
    "snapshot": {
      "languages": [
        "javascript",
        "python",
        "typescript"
      ],
      "top_level_dirs": [
        ".chau7",
        ".chunk-history",
        ".claude",
        ".gcloud_tmp",
        ".githooks",
        ".github",
        ".husky",
        ".hypothesis",
        ".lighthouseci",
        ".playwright-mcp",
        ".pnpm-store",
        ".storybook",
        ".wrangler",
        "Agents",
        "TODO",
        "alerts",
        "apps",
        "backend",
        "catalog",
        "config",
        "contracts",
        "devops",
        "docker",
        "docs",
        "e2e",
        "functions",
        "gcp-run-proxy",
        "grafana-provisioning",
        "load_tests",
        "logs",
        "output",
        "packages",
        "patches",
        "playwright-report",
        "project",
        "public",
        "scripts",
        "shared",
        "src",
        "stories",
        "test-results",
        "tests",
        "tools"
      ],
      "readme_path": "tools/mcp-mordor/README.md"
    },
    "files_count": 106096,
    "functions_count": 12763,
    "classes_count": 3255,
    "docs_count": 1085,
    "configs_count": 79
  },
  "signals": {
    "boundary_clarity": {
      "score": 68,
      "level": "mixed",
      "evidence": [
        "cross-area semantic edges: 41827/264266",
        "source files with area assignment: 5350/5369",
        "generic source file names: 13"
      ]
    },
    "entrypoint_clarity": {
      "score": 100,
      "level": "strong",
      "evidence": [
        "direct code entrypoint edges: 1192",
        "configs with entrypoints: 6",
        "areas with ambiguous entrypoints: 1"
      ]
    },
    "config_hygiene": {
      "score": 21,
      "level": "weak",
      "evidence": [
        "operational configs: 39",
        "linked configs: 39/39",
        "duplicate config families: 26"
      ]
    },
    "hidden_coupling": {
      "score": 23,
      "level": "weak",
      "evidence": [
        "low-confidence semantic edges: 209164/235410",
        "high-confidence semantic edges: 14639/235410",
        "cross-area semantic edges: 30948/235410"
      ]
    },
    "parser_visibility": {
      "score": 87,
      "level": "strong",
      "evidence": [
        "supported source files: 5099/5369",
        "source files with semantic extraction: 3816/5369",
        "total extracted functions/classes: 16018"
      ]
    }
  },
  "overview": {
    "overview_docs": [
      ".claude/README.md",
      "Agents/Skills Manager/README.md",
      "Agents/skills/README.md"
    ],
    "code_areas": [
      "backend",
      "packages",
      "scripts"
    ],
    "reference_areas": [
      "docs",
      "test-results"
    ],
    "subareas": [
      "backend/accounts",
      "backend/core",
      "packages/app-shared",
      "packages/ui"
    ],
    "entrypoints": [
      "apps/customer/menu.config.ts",
      "apps/customer/sw.ts",
      "apps/mordor/menu.config.ts"
    ],
    "key_configs": [
      "backend/pyproject.toml",
      "packages/auth/package.json"
    ],
    "representative_code_files": [
      "Agents/skills/_meta/scripts/add_frontmatter.py",
      "Agents/skills/_meta/scripts/analyze_repo.py",
      "Agents/skills/_meta/scripts/analyze_usage_logs.py",
      "Agents/skills/_meta/scripts/build_learning_index.py",
      "Agents/skills/_meta/scripts/build_skills_registry.py"
    ],
    "representative_docs": [
      ".claude/README.md",
      "Agents/Skills Manager/README.md",
      "Agents/skills/README.md",
      "Agents/skills/architecture/SKILL.md",
      "Agents/skills/architecture/references/adr-index.md"
    ]
  },
  "anchors": [
    {
      "kind": "file",
      "id": "tools/mcp-mordor/README.md",
      "file": "tools/mcp-mordor/README.md",
      "reason": "repository readme"
    },
    {
      "kind": "file",
      "id": "docs/adr/010-monorepo-architecture.md",
      "file": "docs/adr/010-monorepo-architecture.md",
      "reason": "architecture document"
    },
    {
      "kind": "folder",
      "id": "packages",
      "file": null,
      "reason": "top-level area"
    },
    {
      "kind": "folder",
      "id": "tools",
      "file": null,
      "reason": "top-level area"
    },
    {
      "kind": "file",
      "id": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "file": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "reason": "likely entrypoint"
    }
  ],
  "in_scope": {
    "files": [
      {
        "value": "tools/mcp-mordor/README.md",
        "kind": "file",
        "reason": "anchor-adjacent file"
      }
    ],
    "symbols": [],
    "areas": [
      {
        "value": "packages",
        "kind": "area",
        "reason": "primary top-level area"
      },
      {
        "value": "tools",
        "kind": "area",
        "reason": "primary top-level area"
      }
    ]
  },
  "out_of_scope": {
    "files": [],
    "symbols": [],
    "areas": []
  },
  "dependencies": [
    {
      "from": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "to": "Agents/skills/_meta/scripts/add_frontmatter.py::generate_frontmatter",
      "kind": "defines"
    },
    {
      "from": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "to": "Agents/skills/_meta/scripts/add_frontmatter.py::has_frontmatter",
      "kind": "defines"
    },
    {
      "from": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "to": "Agents/skills/_meta/scripts/add_frontmatter.py::main",
      "kind": "defines"
    },
    {
      "from": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "to": "Agents/skills/_meta/scripts/add_frontmatter.py::process_file",
      "kind": "defines"
    },
    {
      "from": "Agents/skills/_meta/scripts/analyze_repo.py",
      "to": "Agents/skills/_meta/scripts/analyze_repo.py::RepoAnalyzer",
      "kind": "defines"
    },
    {
      "from": "Agents/skills/_meta/scripts/analyze_repo.py",
      "to": "Agents/skills/_meta/scripts/analyze_repo.py::__init__",
      "kind": "defines"
    }
  ],
  "impact": [
    {
      "symbol": "apps/customer/menu.config.ts",
      "file": "apps/customer/menu.config.ts",
      "reason": "entrypoint candidate"
    },
    {
      "symbol": "apps/customer/sw.ts",
      "file": "apps/customer/sw.ts",
      "reason": "entrypoint candidate"
    }
  ],
  "snippets": [
    {
      "file": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "start_line": 1,
      "end_line": 20,
      "kind": "overview"
    },
    {
      "file": "docs/adr/010-monorepo-architecture.md",
      "start_line": 1,
      "end_line": 20,
      "kind": "overview"
    },
    {
      "file": "tools/mcp-mordor/README.md",
      "start_line": 1,
      "end_line": 20,
      "kind": "overview"
    }
  ],
  "risk_flags": [
    {
      "scope": ".gcloud_access_token",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": ".github/workflows/migrations-guard.yml",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": ".pnpm-store/v10/index/17/3659f9b86de57d0529eeccc33dc3015026947d415796e549a93f9473012b3d-oauth4webapi@3.8.2.json",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": ".pnpm-store/v10/index/18/7b8344ed764b2a6ed9c57bd1dd5d900d845265c7827b6bcdba6f381f90cbee-comma-separated-tokens@1.0.8.json",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": ".pnpm-store/v10/index/29/afbd4ebbadbfb1bc33a593e927a2456cfbf762b9a84a881841b35ca84013ac-class-variance-authority@0.7.1.json",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": ".pnpm-store/v10/index/45/d2547e5704ddc5332a232a420b02bb4e853eef5474824ed1b7986cf8473789-js-tokens@4.0.0.json",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": ".pnpm-store/v10/index/55/dffd1150e2bba3cf26df72021eaba193fa125d711eb76f2151a3c81b074744-@csstools+css-tokenizer@3.0.4.json",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": ".pnpm-store/v10/index/59/dee61cf43ff33cba423edfe13e3abe0ddaa28afc7ec9099ba8366728f4eb8a-@auth+core@0.41.0.json",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": ".pnpm-store/v10/index/9b/16bd13d21314eb746da9f78fa2f93298f07a01b3ea505098cd4826459e0591-js-tokens@9.0.1.json",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": ".pnpm-store/v10/index/a3/69ee27ce43e04491c9b877cdb0390e5d4e7b5edf4592fefd0d7b6f5a90752f-@auth0+auth0-react@2.5.0.json",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": ".pnpm-store/v10/index/ab/f25255dd4ba6dce17f96e4626e286f88963e3c742a245edec44504dad5a9b2-space-separated-tokens@1.1.5.json",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": ".pnpm-store/v10/index/e1/7bf1d84e0dd808abaf5469f8a39e8dd0dba63e4b9df2ed359fd368e768ed56-@auth0+auth0-spa-js@2.5.0.json",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": ".pnpm-store/v10/index/f9/ce7582ab8cdc5ea73159a802eb1127b448a18d0ae13b3d1c20b0cb2fc14687-next-auth@5.0.0-beta.30.json",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": ".pnpm-store/v10/index/ff/b05db84885788349ee695cf22466aa9d2c0f0d9ada50056a18a0fd11a9a67e-eslint-plugin-no-secrets@2.2.1.json",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": ".secrets.baseline",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "Agents/skills/auth/SKILL.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "Agents/skills/auth/references/api-endpoints.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "Agents/skills/auth/references/api-keys.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "Agents/skills/auth/references/authentication.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "Agents/skills/auth/references/common-patterns.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "Agents/skills/auth/references/database-tables.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "Agents/skills/auth/references/decisions.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "Agents/skills/auth/references/learn-log.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "Agents/skills/auth/references/rbac.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "Agents/skills/auth/references/rbac.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "Agents/skills/auth/references/security.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "Agents/skills/auth/references/troubleshooting.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "Agents/skills/ci-deploy/SKILL.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "Agents/skills/ci-deploy/references/advanced-pipelines.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "Agents/skills/ci-deploy/references/decisions.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "Agents/skills/ci-deploy/references/docker.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "Agents/skills/ci-deploy/references/gcp.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "Agents/skills/ci-deploy/references/kubernetes.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "Agents/skills/ci-deploy/references/learn-log.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "Agents/skills/ci-deploy/references/pipelines.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "Agents/skills/ci-deploy/references/secrets.md",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "Agents/skills/ci-deploy/references/secrets.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "Agents/skills/database/references/migrations.md",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "Agents/skills/integrations/references/oauth-flows.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "Agents/tasks/2025-01-13-integrations-onboarding-oauth.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "Agents/tasks/celery-cloudbuild-deploy.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "Agents/tasks/celery-redis-secret-wiring.md",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "Agents/tasks/dedicated-repo-migration.md",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "Agents/tasks/fix-bootstrap-permission-case.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "Agents/tasks/fix-environment-discovery-migration.md",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "Agents/tasks/fix-mordor-roles-permissions-404.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "Agents/tasks/fix-preauth-error-production.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "Agents/tasks/google-oauth-onboarding.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "Agents/tasks/merge-environment-0036-migrations.md",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "Agents/tasks/otel-step1-deployment.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "Agents/tasks/rbac-implementation-plan-intake.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "Agents/tasks/rbac-pr5-pr8.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "Agents/tasks/rbac-role-management-cleanup.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "Agents/tasks/rbac-role-management-permissions.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "Agents/tasks/role-management-permissions-check.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "apps/customer/src/entry-authenticated.tsx",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "apps/mordor/src/entry-authenticated.tsx",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "apps/organizations/src/entry-authenticated.tsx",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/MIGRATION_SCRIPT.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/admin_rbac_api_views.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/admin_rbac_views.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/auth0_management.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/accounts/auth_analytics_models.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/accounts/auth_analytics_serializers.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/accounts/auth_analytics_views.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/accounts/management/commands/rbac_dump_casl_catalog.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/management/commands/rbac_lifecycle_tick.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/management/commands/rbac_roles_summary.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/management/commands/rbac_seed_permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/middleware_auth_enforcement.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/accounts/middleware_rbac_identity.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0002_organization.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0003_userprofile_org_default.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0004_rls_userprofile.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0005_tenant_membership.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0006_userprofile_tenant_nullable.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0007_seed_default_tenants_assign.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0008_userprofile_tenant_nonnull.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0009_rls_userprofile_tenant_update.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0010_alter_userprofile_organization_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0011_profile_identity_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0012_profile_phone_split.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0013_team_and_identity_extras.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0014_team_id_default.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0015_userprofile_notification_prefs.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0016_userprofile_tz_locale_notif_state.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0017_tenant_notification_policy.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0018_tenant_lifecycle_and_admin_models.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0019_plan_entitlements.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0020_alter_plandefinition_id_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0021_internal_scopes_and_profile_flag.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0022_custom_attributes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0023_team_user_custom.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0024_rbac_registry.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/0024_rbac_registry.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0025_role_archive.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0025_search_trgm_indexes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0026_alter_customattributedefinition_id.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0027_merge_20250922_0837.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0028_permission_meta.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/0028_permission_meta.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0028_role_risk_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0029_permission_metadata.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/0029_permission_metadata.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0030_userprofile_ui_prefs.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0031_enable_tenant_rls.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0032_organization_hierarchy.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0033_remove_organization_org_parent_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0034_check_constraints.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0035_organization_profile_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0036_grc_organization_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0037_remove_sso_mfa_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0038_alter_organization_tax_id.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0039_tenant_api_calls_month_tenant_api_calls_today_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0040_tenant_admin_notification_message_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0041_rolev2_organization_parent_userprofile_primary_team_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0042_tenanthealthalertrule_tenanthealthmetric_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0043_broadcasttemplate_scheduledbroadcast_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0044_alter_scopedpermission_resource_roletemplate.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/0044_alter_scopedpermission_resource_roletemplate.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0045_rolev2_tags.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0046_remove_business_unit_and_update_team_types.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0047_sync_rbac_permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/0047_sync_rbac_permissions.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0048_remove_userprofile_role.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0049_alter_scopedpermission_resource.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/0049_alter_scopedpermission_resource.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/0999_rename_rolev2_to_role.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1000_alter_role_options_alter_role_tenant.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1001_drop_legacy_rbac_tables.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/1001_drop_legacy_rbac_tables.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1002_recreate_role_permissions_through_table.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/1002_recreate_role_permissions_through_table.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1003_alter_scopedpermission_resource.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/1003_alter_scopedpermission_resource.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1004_alter_scopedpermission_action.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/1004_alter_scopedpermission_action.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1005_add_device_and_session_models.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1006_alter_scopedpermission_resource.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/1006_alter_scopedpermission_resource.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1007_add_dashboard_resource.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1008_entitlements_catalog.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1009_seed_owner_internal.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1010_subscription_split.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1011_alter_catalogsubscription_id_alter_creditgrant_id_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1012_merge_20251105_2056.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1013_delete_rolev2_remove_role_archived_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1014_notification_columns_and_locale_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1015_merge_20251122_2008.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1016_add_account_models.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1017_assign_demo_admin.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1018_remove_demo_fullaccess_prod.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1019_add_integrations_permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/1019_add_integrations_permissions.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1020_add_user_search_trgm_indexes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1021_role_risk_level_role_risk_meta_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1022_userprofile_rls_by_user_id.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1023_standardize_rls_gucs.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1024_account_assetentity_fk.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/1025_account_roles_permissions_auth.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1026_remove_account_accounts_ac_tenant__auth_idx_and_more.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/accounts/migrations/1026_remove_account_accounts_ac_tenant__auth_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1027_add_risk_rules_permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/1027_add_risk_rules_permissions.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1028_add_account_risk_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1029_add_finding_template_model.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1030_role_is_template_role_source_template_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1031_role_templates_global.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1032_remove_role_accounts_role_template_requires_null_tenant_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1033_drop_scopedpermission_and_legacy_role_fields.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/1033_drop_scopedpermission_and_legacy_role_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1034_alter_userroleassignment_scope_type_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1035_roleriskpolicy.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1036_account_oauth_scopes.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/accounts/migrations/1036_account_oauth_scopes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1037_add_external_avatar_url.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1038_grant_demo_admin_v3.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1039_rbac_homogenization.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/1039_rbac_homogenization.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1040_remove_permissionauditlog_actor_and_more.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/1040_remove_permissionauditlog_actor_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1041_alter_role_permissions_v3.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/1041_alter_role_permissions_v3.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1042_migrate_permissions_to_canonical.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/1042_migrate_permissions_to_canonical.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1043_access_grants_and_scope_types.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1044_tenant_slug_global_unique.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1045_rename_accounts_acc_grantor_status_idx_accounts_ac_grantor_970445_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1046_tenant_onboarding_apps_score_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1047_tenant_dns_discovery_seed_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1048_authanalyticssummary_authevent.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/accounts/migrations/1048_authanalyticssummary_authevent.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1048_seed_free_plan.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1049_add_domain_role_exposure.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1050_change_domain_role_to_roles_array.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1051_tenant_profiles.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1052_seed_tenant_profiles.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1053_tenant_profile_templates.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1054_seed_tenant_profile_templates.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1055_role_templates_scope_and_profiles.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1056_alter_role_organization_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1057_tenantdomain_asset_entity.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1058_role_template_visibility_and_auto_create.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1059_fix_account_asset_fk_constraint.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1060_enforce_userprofile_rls.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1061_external_groups.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1062_rename_accounts_ex_tenant__3a632a_idx_accounts_ex_tenant__0c1f4d_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1063_role_is_platform_staff.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1064_platform_roles.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1065_usersession_realm_enforcement.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1066_remove_platformroleassignment_platform_role_assignment_user_role_uniq_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1067_consolidate_data_models.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1068_alter_organization_options_alter_team_options_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1069_documentslot_and_status.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1070_platform_role_assignment_starts_at.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1071_merge_20260202_1350.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1072_seed_default_platform_roles.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1073_feature_key_allow_dots.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1074_aeptus_support_access.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1075_alter_usertenantmembership_role.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1076_userprofile_rls_insert_policy.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1077_migrate_platform_roles_to_canonical_rbac.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/migrations/1077_migrate_platform_roles_to_canonical_rbac.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1078_userprofile_rls_include_memberships.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1079_userprofile_archived_at.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1080_profile_integrity_jobs.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1081_impersonation_ticket_and_request_id.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1082_alter_tenant_options.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1083_alter_scheduledbroadcast_status.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1084_tenant_profile_fk_and_framework_template.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1085_seed_baseline_framework_templates.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/1086_merge_20260305_1932.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/FOLDER.migrations.md",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/accounts/permissions_base.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/rbac.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/rbac_audit_models.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/rbac_canonical.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/rbac_helpers.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/rbac_models.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/rbac_permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/rbac_scope.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/rbac_signals.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/tests/test_rbac_access_engine.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/tests/test_rbac_lifecycle_tick.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/tests/test_rbac_on_behalf_audit.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/accounts/tests/test_rbac_team_auto_assign.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/adn/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/0002_enable_rls.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/0003_fix_category_slug_uniqueness.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/0004_pipelinerun_enrichmentqueue_directorysignal_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/0005_localproviderentry_localserviceentry_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/0006_remove_localproviderentry_unique_local_provider_domain_per_tenant_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/0007_add_schema_version.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/0008_directorycategory_expected_at_onboarding.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/0009_add_app_metadata_facts.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/0010_expand_fact_types.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/0011_add_category_owner_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/0012_pipelinerun_add_adn_onboarding_enrich_stage.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/0013_category_owner_delegation.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/0014_pipelinestageconfig.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/0015_remove_directoryfact_fact_single_target_entity_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/0016_sitemap_supply_chain_choice_expansions.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/0017_rename_enrichmentqueue_pipelinequeue.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/0018_rename_adn_pipelin_target__70e8a1_idx_adn_pipelin_target__d13f85_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/0019_pipelinebatch.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/0020_rename_adn_pipelin_status_batch_idx_adn_pipelin_status_90c11e_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/adn/permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/adn/tests/test_permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/ai_providers/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/ai_providers/migrations/0002_seed_providers.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/ai_providers/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/analytics/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/analytics/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/api_keys/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/api_keys/migrations/0002_rename_api_keys_tenant__a3f8b1_idx_api_keys_tenant__aa40c3_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/api_keys/migrations/0003_unique_constraints.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/api_keys/migrations/0004_apikey_user.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/api_keys/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/api_usage/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/api_usage/migrations/0002_enable_rls.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/api_usage/migrations/0003_rename_api_deprec_tenant_status_idx_api_depreca_tenant__60e9d0_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/api_usage/migrations/0004_brin_indexes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/api_usage/migrations/0005_merge_20251001_1316.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/api_usage/migrations/0006_standardize_rls_gucs.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/api_usage/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/audit/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/audit/migrations/0002_rls_and_brin.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/audit/migrations/0003_dedup_unique.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/audit/migrations/0003_partition_shadow_table.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/audit/migrations/0004_alter_auditeventv2_options_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/audit/migrations/0004_audit_export_job.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/audit/migrations/0005_audit_ingest_keys.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/audit/migrations/0005_audit_phase3_enhancements.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/audit/migrations/0006_auditpolicy_legal_hold.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/audit/migrations/0007_auditpolicy_retention_status.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/audit/migrations/0008_auditeventv2_perf_indexes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/audit/migrations/0009_merge_0004_0008.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/audit/migrations/0010_drop_audit_event_legacy.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/audit/migrations/0011_alter_auditexportjob_id.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/audit/migrations/0012_auditexportjob_expires_at_auditexportjob_format_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/audit/migrations/0013_standardize_rls_gucs.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/audit/migrations/0014_actor_id_string.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/audit/migrations/FOLDER.migrations.md",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/audit/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/automations/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/automations/migrations/0002_definition.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/automations/migrations/0003_data_model_rest.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/automations/migrations/0004_run_logs.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/automations/migrations/0005_enable_rls.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/automations/migrations/0006_check_constraints.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/automations/migrations/0007_performance_indexes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/automations/migrations/0008_brin_indexes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/automations/migrations/0009_partial_indexes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/automations/migrations/0010_remove_automationdefinition_auto_def_tenant_status_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/automations/migrations/0011_merge_20251106_2056.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/automations/migrations/0012_remove_automationdefinition_created_by_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/automations/migrations/0013_standardize_rls_gucs.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/automations/migrations/0014_eventdeadletter_payload_json.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/automations/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/collaboration/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/collaboration/migrations/0002_review_models.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/collaboration/migrations/0003_rename_collaborati_locatio_idx_collaborati_locatio_8dcb41_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/collaboration/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/community/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/community/migrations/0002_enable_rls_policies.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/community/migrations/0003_alter_implicitsignal_target_type.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/community/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0002_performance_indexes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0003_custom_dashboards.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0004_remove_controldefinition_ctrl_tenant_status_domain_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0005_add_control_assessment_items.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0006_add_scope_dsl_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0007_rename_ctrl_item_tenant_occ_idx_controls_co_tenant__9e7728_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0008_access_review_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0009_item_validity_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0009_rename_controls_co_tenant_c_kind_idx_controls_co_tenant__9e629c_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0010_merge_20251007_1220.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0010_occurrence_signoff_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0011_merge_20251007_1252.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0012_rename_controls_occ_signoff_due_idx_controls_co_signoff_7bb034_idx.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0013_controldefinition_business_unit_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0014_add_composite_indexes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0014_add_performance_indexes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0015_merge_20251104_0914.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0016_evidence_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0017_add_search_vector_control.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0018_controldefinition_idx_control_search.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0019_controldefinition_idx_control_search.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0020_enable_rls.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0021_evidence_artifact_scan_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0022_framework_requirement_and_policy_mapping.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0023_populate_framework_requirements.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/0024_rename_controls_fr_framewo_idx_cat_controls_fr_framewo_83042f_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/controls/permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/controls/tests/test_permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/controls/tests/test_rbac_boundary.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/core/management/commands/create_search_permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/core/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0003_inapp_security_evidence.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0004_alerts_evidence_meta_url.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0005_auditevent_healthcheck_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0006_outbound_email_job.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0007_emailjob_partial_idx.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0008_outbound_email_bodyhash_unique.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0009_remove_outboundemailjob_core_emailjob_triplet_uniq_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0010_pg_stat_statements_extension.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0011_delete_auditevent.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0012_drop_core_auditevent.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0013_rlsauditevent.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0014_designsystempage_designsystemcomponent_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0015_add_planned_components.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0016_add_resource_permission_models.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/core/migrations/0016_add_resource_permission_models.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0017_rlsauditevent.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0018_change_default_visibility_to_tenant.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0019_tenantattribute_moduleattributeconfig_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0020_queryperformancelog_queryperformancestats.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0021_rename_core_queryp_created_af2bd6_idx_core_queryp_created_ff0917_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0022_merge_20251106_2056.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0023_search_analytics_models.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0024_alter_searchanalytics_created_by_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0025_export_job.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0026_enable_rls_core_export_job.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/0027_alter_exportjob_format_alter_exportjob_status.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/FOLDER.migrations.md",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/core/permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/core/permissions/__init__.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/core/permissions/decorators.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/core/permissions/helpers.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/core/permissions/policy.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/core/permissions/rbac.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/core/permissions/rls_queryset_manager.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/core/permissions/test_utils.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/core/permissions/tests/__init__.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/core/permissions/tests/test_decorators.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/core/permissions/tests/test_helpers.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/core/permissions/tests/test_policy.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/core/tests/test_search/test_search_permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/deployment-guide.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "backend/directory/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/directory/migrations/0002_bitemporal_constraints.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/directory/migrations/0003_add_service_offering_technology.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/directory/migrations/0004_bitemporal_exclusion_constraints.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/directory/migrations/0005_check_constraints.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/directory/migrations/0006_alter_technologycomponent_unique_together_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/directory/migrations/0007_service_offering_technology_constraints_and_cleanup.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/directory/migrations/0008_legalentity_categories_serviceoffering_categories_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/directory/migrations/0009_legalentity_serviceoffering_expansion.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/directory/migrations/0010_legalentity_industry_sanctions_jurisdiction.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/directory/migrations/0011_allow_null_legal_entity_on_service_offering.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/directory/migrations/0012_remove_technologycomponent_categories_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/directory/migrations/0013_fix_techcat_null_distinct.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/directory/migrations/0014_remove_serviceoffering_tags.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/directory/migrations/0015_technologyproduct_categories.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/directory/migrations/0016_technologycategory_is_active.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/directory/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/documents/deployment-guide.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "backend/documents/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/documents/migrations/0002_rename_doc_t_type_deleted_idx_documents_d_tenant__5ef7dd_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/documents/migrations/0003_enable_rls.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/documents/migrations/0004_expand_doctype_and_relations.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/documents/migrations/0005_documentslot_and_status.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/documents/migrations/0006_documenttypeprofile.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/documents/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0002_add_owned_resource_mixin.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0002_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0002_riskrule_riskrulefielddefinition_riskruleexecution.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0003_add_business_security_ownership.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0004_alter_asset_visibility_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0005_remove_asset_criticality_asset_service_asset_tier.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0006_add_composite_indexes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0006_add_performance_indexes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0007_merge_20251104_0914.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0008_remove_asset_env_asset_lifecycle_risk_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0009_merge_20251106_2056.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0010_remove_asset_environment_owner_t_925258_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0011_asset_idx_asset_type_tier_stat_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0012_add_search_vector_asset.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0013_asset_idx_asset_search.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0014_asset_managed_by_thirdpartyentity.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0015_alter_asset_unique_together_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0017_bitemporal_table_maintenance_tuning.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0018_asset_constraints_and_assettechnology_pair_unique.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0019_enable_rls.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0020_standardize_risk_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0021_merge_20251216_1225.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0022_rename_env_riskrule_tenant_target_idx_environment_tenant__8ca752_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0023_riskrulefielddefinition_category.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0024_add_asset_risk_breakdown.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0025_sprint14_asset_enhancements.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0026_rename_env_compmap_t_stat_idx_environment_tenant__24f617_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0026_update_asset_search_vector_business_unit_option.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0027_merge_20251224_0905.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0028_asset_hosting_model_asset_local_service_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0029_asset_data_model_v12_1.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0030_remove_asset_idx_asset_category_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0031_risk_rule_library.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0032_remove_orgrulevisibility_unique_org_rule_visibility_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0033_add_asset_domain_registration_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0034_alter_asset_asset_type.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0035_domain_analyzer_integration_sprint16.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0036_asset_discovery_tracking_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0036_rename_idx_certhistory_asset_environment_tenant__3daa83_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0037_merge_20260109_0700.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0038_threat_intelligence_traffic_ranking.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0039_remove_asset_idx_asset_threat_malicious_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0040_technology_fingerprinting.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0041_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0042_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0043_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0044_asset_discovery_sources.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0045_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0046_remove_threatintelligencecheck_uniq_threat_tenant_domain_active_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0047_update_threatintelligencecheck_constraint.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0048_backfill_asset_discovery_sources.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0049_asset_directory_category.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0050_remove_technologycategory_parent_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/0051_remove_asset_business_function_id_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/environment/permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/environment/risk_rule_library_permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/environment/risk_rule_permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/events/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/events/migrations/0002_add_owned_resource_mixin.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/events/migrations/0003_add_business_security_ownership.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/events/migrations/0004_alter_event_visibility_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/events/migrations/0005_incident.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/events/migrations/0006_delete_incident.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/events/migrations/0007_incident_assetentity.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/events/migrations/0008_alter_incident_created_at_alter_incident_created_by_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/events/migrations/0009_alter_incident_business_owner_team_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/events/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/frameworks/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/gcp/deploy.sh",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "backend/gcp/setup-search-infrastructure.sh",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "backend/guide-migrations.md",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/information/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/information/migrations/0001_initial_ims_models.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/information/migrations/0002_alter_document_content_type_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/information/migrations/0003_merge_20251106_2056.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/information/migrations/0004_assetentity_fks_and_contenttypes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/information/migrations/0005_alter_privacyprofile_content_type.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/information/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/information/models/migration.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/information/serializers/migration.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0002_alter_integrationconnection_provider.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0003_slackinstall.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0004_rename_integrations_slack_tenant_team_idx_integration_tenant__f6ace6_idx.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0005_enhance_integrationconnection_for_adapter_pattern.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0006_enhance_integration_connection.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0007_integrationfieldmapping.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0008_scaling_architecture.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0009_alter_integrationprovider_options_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0010_integrationaction_integrationdatapoint.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0011_seed_google_workspace_actions_complete.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0012_integrationaction_category.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0013_seed_google_workspace_webhooks.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0014_seed_slack_provider_and_actions.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0015_seed_github_provider_and_actions.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0016_add_is_automation_enabled.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0017_integrationaction_integration_auto_en_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0018_integration_sync_history.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0019_add_sync_history_data_snapshots.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0020_add_sync_type_choices.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0021_rename_nango_connection_id.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0022_normalize_integration_provider_categories.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0023_seed_microsoft_365_provider.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0024_seed_microsoft_teams_provider.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0025_normalize_connected_status_to_active.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/0026_seed_google_workspace_provider.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/integrations/tests/test_token_lifecycle_guardrails.py",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "backend/integrations/tests/test_token_refresh.py",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "backend/integrations/token-lifecycle-standard.md",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "backend/knowledge/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/knowledge/migrations/0002_remove_controlmapping_unique_policy_requirement_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/knowledge/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/localization/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/localization/migrations/0002_add_owned_resource_mixin.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/localization/migrations/0003_add_business_security_ownership.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/localization/migrations/0004_alter_glossaryterm_visibility_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/localization/migrations/0005_add_analytics_models.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/localization/migrations/0006_alter_translationchangelog_created_by_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/localization/migrations/0007_translation_ai_config.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/localization/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/manual-deploy-with-verify.sh",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "backend/mapping_intelligence/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/mapping_intelligence/migrations/0002_add_missing_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/mapping_intelligence/migrations/0002_fielddefinition_mapping_int_synonym_49b140_gin.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/mapping_intelligence/migrations/0003_add_aimachinesettings.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/mapping_intelligence/migrations/0003_fielddefinition_tenant_scope.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/mapping_intelligence/migrations/0003_rename_mapping_int_entity__idx_mapping_int_entity__9ab5a0_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/mapping_intelligence/migrations/0004_merge_20251020_1449.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/mapping_intelligence/migrations/0005_add_versioning_and_constraints.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/mapping_intelligence/migrations/0007_add_performance_indexes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/mapping_intelligence/migrations/0008_merge_20251106_2056.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/mapping_intelligence/migrations/0009_mappinghistory_updated_at_mappinghistory_updated_by_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/mapping_intelligence/migrations/0010_merge_20260105_1105.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/mapping_intelligence/migrations/0011_remove_fielddefinition_mapping_int_entity__030d87_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/mapping_intelligence/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/mapping_intelligence/permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/menu_overrides/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/menu_overrides/migrations/0002_add_navigation_analytics.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/menu_overrides/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/middleware/rbac_enforcement.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/onboarding/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/onboarding/migrations/0002_onboardingruntimestate.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/onboarding/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/operational/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/operational/migrations/0002_event_sourcing_triggers.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/operational/migrations/0003_fix_event_sourcing_trigger.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/operational/migrations/0004_trigger_request_id.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/operational/migrations/0005_trigger_request_id_metadata.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/operational/migrations/0006_trigger_update_merge_guard.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/operational/migrations/0007_trigger_merge_guard_jsonb.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/operational/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/ops/scripts/deploy-celery-jobs.sh",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "backend/ops/scripts/deploy-rbac-seed.sh",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/ops/scripts/deploy-rbac-seed.sh",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "backend/ops/scripts/execute-rbac-seed.sh",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/ops/scripts/run-migrations.sh",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/ops/scripts/seed-rbac-permissions.sh",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/page_actions/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/page_actions/migrations/0002_remove_customaction_unique_custom_action_per_org_page_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/page_actions/migrations/0003_standardize_rls_gucs.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/page_actions/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/page_actions/permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/page_actions/services/permission_service.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/page_actions/tests/test_permission_service.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/posture/finding_template_library_permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/posture/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/posture/migrations/0002_add_owned_resource_mixin.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/posture/migrations/0003_add_business_security_ownership.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/posture/migrations/0004_alter_campaign_visibility_alter_finding_visibility_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/posture/migrations/0005_add_search_vector_finding.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/posture/migrations/0006_finding_idx_finding_search.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/posture/migrations/0007_alter_campaign_scope_assets_alter_finding_asset_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/posture/migrations/0008_add_finding_likelihood_and_targets.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/posture/migrations/0009_add_finding_template_model.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/posture/migrations/0010_finding_template_library.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/posture/migrations/0011_seed_finding_template_library.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/posture/migrations/0012_rename_posture_ftl_category_status_idx_posture_ftl_cat_status_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/posture/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/run_migrations.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/scripts/audit_rbac_migration.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/scripts/audit_rbac_migration.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/scripts/debug/test_automations_permission_debug.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/scripts/debug/test_rbac_migration.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/scripts/debug/test_rbac_migration.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/setup-auto-deploy.sh",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "backend/tasks/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0002_tasklink.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0003_tasksavedview.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0004_task_tags.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0005_task_comments_watchers.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0006_task_attachment.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0007_checklist_item.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0008_alter_checklistitem_created_at_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0008_task_workflow_sla.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0009_task_provenance.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0010_merge_20251006_1220.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0011_add_owned_resource_mixin.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0016_task_completion_rule.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0017_add_business_security_ownership.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0018_alter_checklistitem_visibility_alter_task_visibility_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0019_add_search_vector_task.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0020_task_idx_task_search.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0021_enable_rls.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0022_add_task_decisions.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0023_convert_task_decision_to_task_type.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0024_enforce_single_pending_task_decision.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/0025_alter_task_status_alter_task_type.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tasks/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/tests/admin/test_admin_notifications_policy_rbac.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/admin/test_admin_permission_audit.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/admin/test_admin_permission_audit_correlation.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/admin/test_admin_roles_rbac_edit_allow.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/admin/test_admin_roles_rbac_edit_deny.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/admin/test_admin_users_rbac_allow.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/audit/test_audit_events_rbac_deny_audit.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/audit/test_audit_export_rbac_deny.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/audit/test_audit_export_rbac_superuser.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/audit/test_audit_rbac.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/audit/test_audit_rbac_endpoints.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/audit/test_audit_registry_billing_vendor_required.py",
      "area": "billing",
      "level": "high",
      "reason": "billing logic"
    },
    {
      "scope": "backend/tests/audit/test_audit_registry_permissions_required.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/critical/test_auth_oidc.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/tests/integration/test_auth0_idp_asset_bootstrap.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/tests/integration/test_collaboration_authorization.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/tests/integration/test_db_viewer_rbac_allow.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/integration/test_schema_permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/integration/test_schema_ui_permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/integration/test_secret_hashing.py",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "backend/tests/integration/test_teams_rbac.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/integration/test_teams_rbac_allow.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/integration/test_thirdparty_authorization.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/tests/integration/test_thirdparty_relationship_authorization.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/tests/security/test_auth_flows_comprehensive.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/tests/security/test_auth_login_ratelimit.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/tests/security/test_auth_logout_csrf.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/tests/security/test_auth_session.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/tests/security/test_impersonation_rbac.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/security/test_rbac_admin_api.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/security/test_rbac_casl_mapping.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/security/test_rbac_forbidden_json.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/security/test_rbac_forbidden_json_shape.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/security/test_rbac_risk_matrix.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/security/test_rbac_risk_recalc_command.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/security/test_rbac_settings_guard.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/tests/security/test_thirdparty_unauth_endpoints.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "backend/tests/suppliers/test_suppliers_reports_rbac.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "backend/thirdparties/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0002_enable_rls_policies.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0003_bitemporal_constraints.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0004_rename_tables_suppliers_to_thirdparties.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0005_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0006_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0007_add_asset_service_offering.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0008_rename_thirdparties_a_tenant__c63003_idx_thirdpartie_tenant__0c77fd_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0009_supplier_graph_view.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0010_add_missing_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0011_enable_rls.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0012_remove_thirdpartyrelationship_thirdparty_rel_unique_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0012_suppliers_saved_view.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0013_asset_service_offering_constraints.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0014_check_constraints.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0015_performance_indexes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0016_partial_indexes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0017_add_frontend_aligned_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0018_add_gdpr_data_privacy_models.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0019_remove_dataprivacycontact_created_by_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0020_add_privacy_enhancements.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0021_remove_dataprivacycontact_created_by_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0022_repair_privacy_columns.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0023_remove_asset_asset_tenant_type_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0024_remove_asset_asset_tenant_type_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0025_alter_document_content_type_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0026_supplierassessment_supplierchangerequest_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0027_dataprivacycontact_dataprivacyprofile_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0028_merge_20251023_1923.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0029_tprm_policy_owner_team_doc_source.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0030_rename_tp_tenant_owner_user_idx_thirdpartie_tenant__4ef8e4_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0031_add_business_security_ownership.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0032_dataprivacycontact_dataprivacyprofile_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0033_alter_thirdparty_visibility.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0034_alter_thirdparty_relationship_types.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0035_thirdparty_frameworks_alter_thirdparty_tags.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0036_alter_thirdparty_tags.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0037_add_composite_indexes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0037_add_performance_indexes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0038_merge_20251104_0914.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0039_remove_directorylinkconfig_tp_link_sync_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0041_search_extensions_and_indexes.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0042_add_search_vector_thirdparty.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0043_thirdparty_idx_thirdparty_search.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0044_thirdparty_entity_versioning.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0045_dataprivacyprofile_third_party_entity.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0046_rename_thirdparty_tenant_entity_idx_thirdpartie_tenant__205199_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0047_standardize_rls_gucs.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0048_alter_directorylinkconfig_linked_legal_entity_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0049_fix_thirdparty_no_overlap_valid_to_infinity.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0050_bitemporal_table_maintenance_tuning.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0051_standardize_risk_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0052_alter_thirdparty_risk_factors_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0053_directorylinkconfig_linked_local_provider.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0054_alter_thirdparty_lifecycle_status.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0055_thirdparty_adn_parity_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0056_functionalrole_industrycodecrosswalk_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0057_seed_functional_roles.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0058_seed_industry_crosswalk.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0059_thirdparty_adn_parity_fields.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0060_supplier_directory_category.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0061_thirdparty_control_frameworks.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0062_migrate_frameworks_m2m.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/0063_alter_thirdparty_frameworks.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/FOLDER.migrations.md",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/thirdparties/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/webhooks/migrations/0001_initial.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/webhooks/migrations/0002_rename_webhooks_de_subscri_f5d8c1_idx_webhooks_de_subscri_f97236_idx_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/webhooks/migrations/0003_unique_constraints.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/webhooks/migrations/0004_add_owned_resource_mixin.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/webhooks/migrations/0005_add_business_security_ownership.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/webhooks/migrations/0006_alter_webhookdelivery_visibility_and_more.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/webhooks/migrations/0007_hash_webhook_secrets.py",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "backend/webhooks/migrations/0007_hash_webhook_secrets.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/webhooks/migrations/0008_alter_webhooksubscription_secret.py",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "backend/webhooks/migrations/0008_alter_webhooksubscription_secret.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "backend/webhooks/migrations/__init__.py",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "contracts/migration-to-schemathesis.md",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "devops/grafana/dashboards/rbac-dashboard.json",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "devops/prometheus/rules/rbac-alerts.yml",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "docs/access-auth.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "docs/agents/context/admin.billing.md",
      "area": "billing",
      "level": "high",
      "reason": "billing logic"
    },
    {
      "scope": "docs/api/rbac-api-reference.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "docs/api/rbac-api.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "docs/api/rbac-openapi.json",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "docs/api/rbac-quick-reference.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "docs/architecture/architecture-deployment.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "docs/architecture/rbac-architecture.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "docs/architecture/security-audit-rbac.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "docs/design-system/automated-deployment-setup.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "docs/design-system/design-tokens-tier-guide.md",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "docs/feature-flags/catalog-key-migration.md",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "docs/feature-specs/admin/page-actions/09-deployment-guide.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "docs/feature-specs/controls/deployment-checklist.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "docs/feature-specs/information/my-environment-rbac-integration.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "docs/feature-specs/rbac/admin-roles-review-and-cleanup.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "docs/feature-specs/rbac/rbac-spec.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "docs/feature-specs/search-deployment-guide.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "docs/guides/authentication-setup.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "docs/guides/cost-optimized-deployment.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "docs/guides/deployment-guide-permissions.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "docs/guides/deployment-guide-permissions.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "docs/guides/multi-tenant-deployment-critical.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "docs/guides/post-deployment-setup.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "docs/guides/post-deployment-verification.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "docs/guides/rbac-admin-guide.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "docs/permissions.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "docs/plans/infrastructure-options-comparison.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "docs/prd/rbac-simplified-design.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "docs/rbac-cache-implementation.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "docs/reference/rbac.yaml",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "docs/reference/reference-rbac-permission-sync.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "docs/runbooks/deploy-admin.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "docs/runbooks/deployment-checklist.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "docs/runbooks/rbac-operations-runbook.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "docs/runbooks/rbac-risk-policy.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "docs/runbooks/runbook-deployment-best-practices.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "docs/runbooks/runbook-production-deployment.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "docs/secret-management-plan.md",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "docs/security/rbac-risk-policy.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "e2e/fixtures/auth.fixture.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "e2e/page-objects/auth/login.page.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "e2e/tests/auth/authentication.spec.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "manual-deployment-steps.md",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "migration-complete.md",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "migration-status.md",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "migrations-applied-success.md",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "packages/app-shared/src/app/AuthenticatedApp.tsx",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/AbilityContext.shared.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/AbilityProvider.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/AbilityProviderRoot.tsx",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/AuthError.tsx",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/FOLDER.auth.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/NoTenantAccess.tsx",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/SessionExpiryWarningProvider.tsx",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/SessionGate.tsx",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/UseEnvironmentPermissions.tsx",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/UseEnvironmentPermissions.tsx",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/__tests__/FOLDER.__tests__.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/__tests__/permissionGrouping.test.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/__tests__/permissionGrouping.test.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/ability.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/can.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/logoutBroadcast.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/logoutClient.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/permissionGrouping.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/permissionGrouping.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/rbac-canonical.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/rbac-canonical.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/session.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/sessionExpiryWarningContext.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/auth/useSessionHeartbeat.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/components/admin/AdminBillingView.tsx",
      "area": "billing",
      "level": "high",
      "reason": "billing logic"
    },
    {
      "scope": "packages/app-shared/src/components/admin/OrgBillingOverviewView.tsx",
      "area": "billing",
      "level": "high",
      "reason": "billing logic"
    },
    {
      "scope": "packages/app-shared/src/components/admin/TenantBillingTab.tsx",
      "area": "billing",
      "level": "high",
      "reason": "billing logic"
    },
    {
      "scope": "packages/app-shared/src/components/admin/roles/BatchPermissionUpdates.tsx",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/components/admin/roles/PermissionConflictDetector.tsx",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/components/admin/roles/PermissionMatrix.tsx",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/components/admin/roles/PermissionMatrixSkeleton.tsx",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/components/admin/roles/PermissionsList.tsx",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/components/admin/roles/__tests__/BatchPermissionUpdates.test.tsx",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/components/auth/PermissionDenied.tsx",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/components/auth/PermissionDenied.tsx",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/constants/rbac-module-settings.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/constants/rbac.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/features/admin/components/AdminBillingView.tsx",
      "area": "billing",
      "level": "high",
      "reason": "billing logic"
    },
    {
      "scope": "packages/app-shared/src/features/admin/components/roles/PermissionConflictDetector.tsx",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/features/admin/components/roles/PermissionMatrix.tsx",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/features/admin/components/roles/PermissionMatrixSkeleton.tsx",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/features/admin/components/roles/permissionConflictRules.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/features/admin/components/roles/permissionMatrix.shared.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/features/admin/hooks/useUsersAndPermissions.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/features/admin/pages/AdminBillingPage.tsx",
      "area": "billing",
      "level": "high",
      "reason": "billing logic"
    },
    {
      "scope": "packages/app-shared/src/features/admin/pages/AdminMigrationDashboardPage.tsx",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "packages/app-shared/src/features/admin/pages/AdminMigrationDashboardPageView.tsx",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "packages/app-shared/src/features/auth/components/PermissionDenied.tsx",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/features/auth/components/PermissionDenied.tsx",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/features/auth/components/index.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/features/auth/index.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/features/auth/utils/ability.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/features/auth/utils/can.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/features/auth/utils/index.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/features/auth/utils/session.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/features/information/hooks/useMigration.ts",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "packages/app-shared/src/features/information/pages/MigrationConflictsPage.tsx",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "packages/app-shared/src/features/information/pages/MigrationDashboardPage.tsx",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "packages/app-shared/src/features/information/pages/MigrationImportPage.tsx",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "packages/app-shared/src/features/org/pages/OrgBillingPage.tsx",
      "area": "billing",
      "level": "high",
      "reason": "billing logic"
    },
    {
      "scope": "packages/app-shared/src/hooks/admin/usePermissionsCatalog.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/hooks/admin/useUsersAndPermissions.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/hooks/information/useMigration.ts",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "packages/app-shared/src/hooks/lib/parsePermissions.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/hooks/permissions/__tests__/useCanAccess.test.tsx",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/hooks/permissions/__tests__/usePermission.test.tsx",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/hooks/permissions/index.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/hooks/permissions/testUtils.tsx",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/hooks/permissions/useAbility.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/hooks/permissions/useCanAccess.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/hooks/permissions/usePermission.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/hooks/useOrgBillingApi.ts",
      "area": "billing",
      "level": "high",
      "reason": "billing logic"
    },
    {
      "scope": "packages/app-shared/src/lib/__tests__/permissions.test.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/lib/permissions.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/lib/personalTokensApi.ts",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "packages/app-shared/src/pages/AuthLogoutPage.tsx",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/pages/platform/AuthAnalyticsPage.impl.tsx",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/pages/platform/AuthAnalyticsPage.tsx",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/preauth/debug.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/preauth/index.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/preauth/network.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/preauth/session.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/preauth/telemetry.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/preauth/theme.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/preauth/types.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/preauth/ui.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/preauth/utils.test.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/preauth/utils.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/router/Unauthorized.tsx",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/tests/admin.billing.a11y.test.tsx",
      "area": "billing",
      "level": "high",
      "reason": "billing logic"
    },
    {
      "scope": "packages/app-shared/src/tests/admin.billing.exportmenu.test.tsx",
      "area": "billing",
      "level": "high",
      "reason": "billing logic"
    },
    {
      "scope": "packages/app-shared/src/tests/admin.billing.mobile.test.tsx",
      "area": "billing",
      "level": "high",
      "reason": "billing logic"
    },
    {
      "scope": "packages/app-shared/src/tests/admin.billing.toolbar.smoke.test.tsx",
      "area": "billing",
      "level": "high",
      "reason": "billing logic"
    },
    {
      "scope": "packages/app-shared/src/tests/admin.users.rbac.banner.test.tsx",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/tests/api.credentials.test.ts",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "packages/app-shared/src/tests/auth.can.test.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/tests/permission.gate.test.tsx",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/tests/router.unauthorized.ui.test.tsx",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/app-shared/src/tests/suppliers.directory.views.rbac.test.tsx",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/app-shared/src/types/rbac.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/auth/package.json",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/auth/src/__tests__/permissionGrouping.test.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/auth/src/__tests__/permissionGrouping.test.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/auth/src/__tests__/rbac-canonical.test.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/auth/src/__tests__/rbac-canonical.test.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/auth/src/ability.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/auth/src/can.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/auth/src/index.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/auth/src/logout/broadcast.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/auth/src/logout/client.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/auth/src/logout/index.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/auth/src/permissionGrouping.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/auth/src/permissionGrouping.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/auth/src/rbac-canonical.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/auth/src/rbac-canonical.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/auth/src/session.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/auth/test-results/junit.xml",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/auth/tsconfig.json",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/auth/tsconfig.tsbuildinfo",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "packages/documentation/migration/page-checklist.json",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "packages/types/src/rbac.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "packages/ui/.ai/design-tokens.json",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "packages/ui/.ai/migration-rules.json",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "packages/ui/src/components/molecules/TokenPicker/TokenPicker.tsx",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "packages/ui/src/components/molecules/TokenPicker/index.ts",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "packages/ui/src/tokens/components.css",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "packages/ui/src/tokens/index.css",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "packages/ui/src/tokens/index.ts",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "packages/ui/src/tokens/primitives.css",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "packages/ui/src/tokens/semantic.css",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "packages/ui/src/tokens/themes/dark.css",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "postgres-18-migration-guide.md",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "rbac-cache-delivery.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "rbac-cache-quickstart.md",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "scripts/checks/check-customer-preauth-no-design-system.mjs",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "scripts/ci/check-endpoint-permissions.mjs",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "scripts/ci/check-permission-metadata.mjs",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "scripts/ci/check-route-permissions.sh",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "scripts/ci/check_migrations.sh",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "scripts/ci/validate-rbac-sync.mjs",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "scripts/deploy-types.cjs",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "scripts/deployment/build-production.sh",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "scripts/design-system/generate-token-json.mjs",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "scripts/migration/audit-page-components.mjs",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "scripts/validate-deployment.sh",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "scripts/validation/validate_permissions.py",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    },
    {
      "scope": "scripts/verify-phase0-deployment.sh",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "scripts/verify_migration.sh",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "tests/contract/consumers/auth.contract.test.ts",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "tools/mcp-mordor/src/tools/rbac.ts",
      "area": "permissions",
      "level": "high",
      "reason": "permission boundary"
    }
  ],
  "navigation_order": [
    "tools/mcp-mordor/README.md",
    "docs/adr/010-monorepo-architecture.md",
    "packages",
    "tools",
    "Agents/skills/_meta/scripts/add_frontmatter.py"
  ],
  "budget": {
    "max_anchors": 5,
    "max_files": 8,
    "max_snippets": 8,
    "dependency_depth": 1,
    "impact_depth": 1
  },
  "confidence": {
    "anchor_confidence": 0.85,
    "scope_confidence": 0.8
  }
}
```

## Explanation

```text
Task: Explain this repo
Languages: javascript, python, typescript
Top-level directories: .chau7, .chunk-history, .claude, .gcloud_tmp, .githooks, .github, .husky, .hypothesis, .lighthouseci, .playwright-mcp, .pnpm-store, .storybook, .wrangler, Agents, TODO, alerts, apps, backend, catalog, config, contracts, devops, docker, docs, e2e, functions, gcp-run-proxy, grafana-provisioning, load_tests, logs, output, packages, patches, playwright-report, project, public, scripts, shared, src, stories, test-results, tests, tools
Files indexed: 106096
Functions indexed: 12763
Classes indexed: 3255
Docs indexed: 1085
Configs indexed: 79
README: .claude/README.md

Code areas:
- backend
- packages
- scripts

Reference areas:
- docs
- test-results

Key subareas:
- backend/accounts
- backend/core
- packages/app-shared
- packages/ui

Key configs:
- backend/pyproject.toml
- packages/auth/package.json

Entrypoints:
- apps/customer/menu.config.ts
- apps/customer/sw.ts
- apps/mordor/menu.config.ts

Representative code:
- Agents/skills/_meta/scripts/add_frontmatter.py
- Agents/skills/_meta/scripts/analyze_repo.py
- Agents/skills/_meta/scripts/analyze_usage_logs.py

Representative docs:
- .claude/README.md
- Agents/Skills Manager/README.md
- Agents/skills/README.md

Navigation order:
- tools/mcp-mordor/README.md
- docs/adr/010-monorepo-architecture.md
- packages
- tools
- Agents/skills/_meta/scripts/add_frontmatter.py
```
