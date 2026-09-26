export type CloudUsageSource = "trial" | "quota" | "overage" | "complimentary";

export interface TranscriptionResult {
  text: string;
  duration_ms: number;
  request_id: string;
  source: CloudUsageSource;
}

export interface PostProcessResult {
  text: string;
  tokens_in: number;
  tokens_out: number;
  request_id: string;
  source: CloudUsageSource;
}

export interface NotesAssistResult {
  text: string;
  tokens_in: number;
  tokens_out: number;
  request_id: string;
  source: CloudUsageSource;
}

export type PostProcessTask = "auto";
export type ModelTier = "mini" | "full";

export interface CloudApiErrorBody {
  kind: "api";
  status: number;
  code: string;
  message: string;
}

export interface CloudNetworkErrorBody {
  kind: "network";
  message: string;
}

export interface CloudMissingAuthBody {
  kind: "missing_auth";
  message: string;
}

export interface CloudWavEncodingBody {
  kind: "wav_encoding";
  message: string;
}

export type CloudErrorBody =
  | CloudApiErrorBody
  | CloudNetworkErrorBody
  | CloudMissingAuthBody
  | CloudWavEncodingBody;
