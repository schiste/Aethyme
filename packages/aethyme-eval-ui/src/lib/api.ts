import type {
  EvalResult,
  Repository,
  EvalRunConfig,
  EvalRunState,
  RepositoryPreparation,
  RepositorySetupRequest,
  RepositorySetupStatus,
} from "./types";

const API_BASE = "/api";

export async function fetchResults(): Promise<EvalResult[]> {
  const res = await fetch(`${API_BASE}/results`);
  if (!res.ok) throw new Error(`Failed to fetch results: ${res.statusText}`);
  return res.json();
}

export async function fetchRepositories(): Promise<Repository[]> {
  const res = await fetch(`${API_BASE}/repositories`);
  if (!res.ok) throw new Error(`Failed to fetch repositories: ${res.statusText}`);
  return res.json();
}

export async function validateRepository(
  target: string,
): Promise<{ valid: boolean; errors: string[] }> {
  const res = await fetch(`${API_BASE}/repositories/validate`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ target }),
  });
  if (!res.ok) throw new Error(`Validation request failed: ${res.statusText}`);
  return res.json();
}

export async function prepareRepository(
  target: string,
): Promise<RepositoryPreparation> {
  const res = await fetch(`${API_BASE}/repositories/prepare`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ target }),
  });
  if (!res.ok) {
    const detail = await res.text();
    throw new Error(`Preparation request failed: ${detail}`);
  }
  return res.json();
}

export async function fetchLatestPreparation(
  target: string,
): Promise<RepositoryPreparation | null> {
  const res = await fetch(`${API_BASE}/repositories/prepare/${target}`);
  if (res.status === 404) return null;
  if (!res.ok) {
    const detail = await res.text();
    throw new Error(`Preparation fetch failed: ${detail}`);
  }
  return res.json();
}

export async function setupRepository(
  request: RepositorySetupRequest,
): Promise<{ success: boolean; taskId?: string; path?: string }> {
  const res = await fetch(`${API_BASE}/repositories/setup`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
  if (!res.ok) {
    const detail = await res.text();
    throw new Error(`Setup request failed: ${detail}`);
  }
  return res.json();
}

export async function fetchSetupStatus(
  taskId: string,
): Promise<RepositorySetupStatus> {
  const res = await fetch(`${API_BASE}/repositories/setup/status/${taskId}`);
  if (!res.ok) {
    const detail = await res.text();
    throw new Error(`Setup status failed: ${detail}`);
  }
  return res.json();
}

export async function indexRepository(
  target: string,
): Promise<{ success: boolean; taskId?: string }> {
  const res = await fetch(`${API_BASE}/repositories/index`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ target }),
  });
  if (!res.ok) throw new Error(`Index request failed: ${res.statusText}`);
  return res.json();
}

export async function checkIndexStatus(
  taskId: string,
): Promise<{ status: string; error?: string; output?: string }> {
  const res = await fetch(`${API_BASE}/repositories/index/status/${taskId}`);
  if (!res.ok) throw new Error(`Status check failed: ${res.statusText}`);
  return res.json();
}

export async function generatePlan(
  config: EvalRunConfig,
): Promise<object> {
  const res = await fetch(`${API_BASE}/plan`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(config),
  });
  if (!res.ok) {
    const detail = await res.text();
    throw new Error(`Plan generation failed: ${detail}`);
  }
  return res.json();
}

export async function launchRun(
  config: EvalRunConfig,
): Promise<EvalRunState> {
  const res = await fetch(`${API_BASE}/run`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(config),
  });
  if (!res.ok) {
    const detail = await res.text();
    throw new Error(`Run launch failed: ${detail}`);
  }
  return res.json();
}

export async function fetchRunStatus(): Promise<EvalRunState> {
  const res = await fetch(`${API_BASE}/run/status`);
  if (!res.ok) throw new Error(`Status check failed: ${res.statusText}`);
  return res.json();
}

export async function checkChau7Status(): Promise<{ available: boolean }> {
  const res = await fetch(`${API_BASE}/chau7/status`);
  if (!res.ok) return { available: false };
  return res.json();
}

export async function fetchChau7Tabs(): Promise<any[]> {
  const res = await fetch(`${API_BASE}/chau7/tabs`);
  if (!res.ok) throw new Error(`Failed to fetch tabs: ${res.statusText}`);
  return res.json();
}

// P1/P10: batch-level endpoints for multi-run aggregation.

export interface BatchSummary {
  batch_id: string;
  last_date: string | null;
  eval_type: string | null;
  target: string | null;
  model: string | null;
  runs_in_batch: number | null;
  distinct_runs: number;
  total_rows: number;
}

export interface VarianceComponents {
  sigma2_cond: number | null;
  sigma2_run: number | null;
  sigma2_judge: number | null;
  sigma2_cond_clamped_to_zero: boolean;
  generalizability_coeff: number | null;
  dominant_noise_source: "run" | "judge" | null;
  n_conditions: number;
  n_rows: number;
  mean_runs_per_condition: number;
  n_judge_samples: number;
  n_runs_needed_for_target: number | null;
  target_reliability: number;
  notes: string[];
}

export interface BatchAggregate {
  batch: Record<string, unknown> | null;
  conditions: Record<string, Record<string, unknown>>;
  comparisons_vs_baseline?: Record<string, unknown>;
  scenario_discrimination?: Record<string, unknown>;
  variance_components?: VarianceComponents;
  judge_overhead?: Record<string, unknown>;
}

export async function fetchBatches(limit = 50): Promise<BatchSummary[]> {
  const res = await fetch(`${API_BASE}/batches?limit=${limit}`);
  if (!res.ok) throw new Error(`Failed to fetch batches: ${res.statusText}`);
  return res.json();
}

export async function fetchBatchAggregate(batchId: string): Promise<BatchAggregate> {
  const res = await fetch(`${API_BASE}/batches/${encodeURIComponent(batchId)}`);
  if (!res.ok) throw new Error(`Failed to fetch batch: ${res.statusText}`);
  return res.json();
}

export interface ProbeRow {
  result_id: string;
  probe_name: string;
  result: {
    skipped?: boolean;
    reason?: string;
    // Navigation probe fields
    first_hit_step?: number | null;
    precision_at_k?: number;
    recall_at_k?: number;
    distractor_rate?: number;
    hits?: string[];
    distractors?: string[];
    k_budget?: number;
    k_examined?: number;
    // Graph-usage probe fields
    aethyme_called?: boolean;
    aethyme_call_count?: number;
    first_aethyme_step?: number | null;
    first_mutating_step?: number | null;
    aethyme_called_before_first_edit?: boolean | null;
  };
  condition: string;
  run_index: number | null;
  created_at: string;
}

export async function fetchBatchProbes(batchId: string): Promise<ProbeRow[]> {
  const res = await fetch(
    `${API_BASE}/batches/${encodeURIComponent(batchId)}/probes`,
  );
  if (!res.ok) throw new Error(`Failed to fetch probes: ${res.statusText}`);
  const data = await res.json();
  return (data.probes ?? []) as ProbeRow[];
}
