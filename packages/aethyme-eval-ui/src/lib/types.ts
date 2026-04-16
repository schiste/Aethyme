export type EvalType =
  | "bug-fix"
  | "bug-fix-1"
  | "explain-repo"
  | "navigation-ctf"
  | "cross-package"
  | "impact-analysis"
  | "feature-localization"
  | "config-audit"
  | "dead-code"
  | "migration";

export type TargetName = "grc" | "mediawiki";

export type ModelName = "haiku" | "sonnet" | "opus" | "gpt-5.4";

export type Condition =
  | "control-cto-off"
  | "control-cto-on"
  | "control"
  | "explore"
  | "leverage"
  | "task-conditioned";

export type Reasoning = "high" | "low";

export interface EvalResult {
  id: string;
  date: string;
  evalType: string;
  target: string;
  model: string;
  condition: string;
  reasoning: string;
  cto: string;
  score: number;
  turns: number;
  toolCalls: number;
  totalTokens: number;
  inputTokens: number;
  outputTokens: number;
  cacheRead: number;
  cacheCreate: number;
  cost: number;
  duration: string;
  fixed: boolean;
  scenario: string | null;
  output: string | null;
  toolBreakdown: string | null;
  prompt: string | null;
  runId: string | null;
  qualityScore?: number | null;
  recalculatedEvalScore?: number | null;
  qualityDeltaVsControl?: number | null;
  tokenRatioVsControl?: number | null;
  timeRatioVsControl?: number | null;
  costRatioVsControl?: number | null;
  scorePer1kTokens?: number | null;
  scorePerMinute?: number | null;
  topTools?: string | null;
}

export type ValidationStatus = "valid" | "invalid" | "unknown" | "checking";

export interface IndexInfo {
  indexed: boolean;
  date: string | null;
  sizeMb?: number;
  path: string;
}

export interface SnippetsInfo {
  present: boolean;
  date: string | null;
  totalSnippets: number;
  aethymeSnippets: number;
  path?: string;
  repo?: string;
}

export interface Repository {
  name: string;
  target: string;
  controlPath: string;
  aethymePath: string;
  setupSource: string | null;
  setupCommit: string | null;
  setupControlDirName?: string | null;
  setupAethymeDirName?: string | null;
  setupDest: string;
  validationStatus: ValidationStatus;
  controlClean: { clean: boolean; issues: string[] };
  aethymeIndex: IndexInfo;
  snippets: SnippetsInfo;
}

export type RunStatus = "idle" | "planning" | "running" | "complete" | "error";

export interface EvalRunConfig {
  evalType: EvalType;
  target: string;
  model: ModelName;
  reasoning: Reasoning;
  windowId?: number;
  preparationId?: string;
  cleanupDelaySeconds?: number;
}

export interface EvalRunState {
  status: RunStatus;
  plan: object | null;
  currentPhase: string | null;
  log: string[];
  error: string | null;
}

export interface RepositoryPreparation {
  id: string;
  target: string;
  ready: boolean;
  createdAt: string;
  path: string;
  errors: string[];
  checks: {
    validation: { valid: boolean; errors: string[] };
    controlClean: { clean: boolean; issues: string[] };
    engineBinary: { exists: boolean; path: string };
    aethymeIndex: IndexInfo;
    snippets: SnippetsInfo;
    controlRepo: { path: string; commit: string | null; dirty: boolean | null };
    aethymeRepo: { path: string; commit: string | null; dirty: boolean | null };
  };
}

export interface RepositorySetupRequest {
  target: string;
  source?: string;
  commit?: string;
  force?: boolean;
}

export interface RepositorySetupStatus {
  status: "queued" | "running" | "complete" | "error";
  target: string;
  source: string;
  commit: string;
  force: boolean;
  path?: string;
  requestPath?: string;
  startedAt?: string;
  finishedAt?: string;
  skipped?: boolean;
  output?: string;
  error?: string;
  preparation?: RepositoryPreparation;
}

export type SortDirection = "asc" | "desc";

export interface SortConfig {
  key: keyof EvalResult;
  direction: SortDirection;
}
