// Typed errors — one class per §13 error code. Protocol failures become the matching class,
// carrying `{ code, message, details }`. Force-required is a `StateConflict` whose
// `details.reason === "force_required"` (spec §8, §13).

export type ErrorDetails = Record<string, unknown> | null;

/** Base class for every orcr error. `code` is the stable §13 code. */
export class OrcrError extends Error {
  readonly code: string;
  readonly details: ErrorDetails;

  constructor(code: string, message: string, details: ErrorDetails = null) {
    super(message);
    this.name = new.target.name;
    this.code = code;
    this.details = details ?? null;
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

export class NotFound extends OrcrError {
  constructor(message: string, details: ErrorDetails = null) {
    super("not_found", message, details);
  }
}
export class InvalidRequest extends OrcrError {
  constructor(message: string, details: ErrorDetails = null) {
    super("invalid_request", message, details);
  }
}
export class StateConflict extends OrcrError {
  constructor(message: string, details: ErrorDetails = null) {
    super("state_conflict", message, details);
  }
  /** True when this conflict is an unmanaged-kill force barrier (`details.reason`). */
  get forceRequired(): boolean {
    return (this.details as Record<string, unknown> | null)?.reason === "force_required";
  }
}
export class Blocked extends OrcrError {
  constructor(message: string, details: ErrorDetails = null) {
    super("blocked", message, details);
  }
}
export class Timeout extends OrcrError {
  constructor(message: string, details: ErrorDetails = null) {
    super("timeout", message, details);
  }
}
export class IntegrationMissing extends OrcrError {
  constructor(message: string, details: ErrorDetails = null) {
    super("integration_missing", message, details);
  }
}
export class TranscriptUnavailable extends OrcrError {
  constructor(message: string, details: ErrorDetails = null) {
    super("transcript_unavailable", message, details);
  }
}
export class EnvironmentError extends OrcrError {
  constructor(message: string, details: ErrorDetails = null) {
    super("environment_error", message, details);
  }
}
export class ServerError extends OrcrError {
  constructor(message: string, details: ErrorDetails = null) {
    super("server_error", message, details);
  }
}

const BY_CODE: Record<string, new (m: string, d?: ErrorDetails) => OrcrError> = {
  not_found: NotFound,
  invalid_request: InvalidRequest,
  state_conflict: StateConflict,
  blocked: Blocked,
  timeout: Timeout,
  integration_missing: IntegrationMissing,
  transcript_unavailable: TranscriptUnavailable,
  environment_error: EnvironmentError,
  server_error: ServerError,
};

/** Reconstruct the right typed error from a wire error object `{code,message,details}`. */
export function errorFromWire(err: unknown): OrcrError {
  const e = (err ?? {}) as Record<string, unknown>;
  const code = typeof e.code === "string" ? e.code : "server_error";
  const message = typeof e.message === "string" ? e.message : "server error";
  const details = (e.details ?? null) as ErrorDetails;
  const Cls = BY_CODE[code] ?? ServerError;
  return new Cls(message, details);
}
