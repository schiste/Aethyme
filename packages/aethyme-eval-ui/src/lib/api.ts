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
