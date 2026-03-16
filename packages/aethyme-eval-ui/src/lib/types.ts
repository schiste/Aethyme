export type EvalType = "bug-fix" | "bug-fix-1" | "explain-repo" | "navigation-ctf" | "cross-package";

export type TargetName = "grc" | "mediawiki";

export type ModelName = "haiku" | "sonnet" | "opus" | "gpt-5.4";

export type Condition =
  | "control-cto-off"
  | "control-cto-on"
  | "control"
  | "explore"
  | "leverage";

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
}

export interface EvalRunState {
  status: RunStatus;
  plan: object | null;
  currentPhase: string | null;
  log: string[];
  error: string | null;
}

export type SortDirection = "asc" | "desc";

export interface SortConfig {
  key: keyof EvalResult;
  direction: SortDirection;
}
