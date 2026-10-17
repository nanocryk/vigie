import { Component, type ErrorInfo, type ReactNode } from "react";

interface State {
  message: string | null;
}

/** Shows a render crash with a way back to the list, instead of a blank page. */
export class ErrorBoundary extends Component<{ children: ReactNode }, State> {
  override state: State = { message: null };

  static getDerivedStateFromError(error: unknown): State {
    return { message: error instanceof Error ? error.message : String(error) };
  }

  override componentDidCatch(error: unknown, info: ErrorInfo): void {
    console.error("render crashed", error, info.componentStack);
  }

  override render(): ReactNode {
    if (this.state.message === null) return this.props.children;
    return (
      <div className="flex min-h-dvh items-center justify-center p-6">
        <div className="w-full max-w-sm rounded-xl border bg-card p-6 text-card-foreground shadow-sm">
          <h1 className="mb-3 text-xl voice-heading text-primary">vigie</h1>
          <div className="space-y-3 text-sm text-muted-foreground">
            <p>Something broke: {this.state.message}</p>
            <button
              type="button"
              className="rounded-md border px-3 py-1.5 text-foreground"
              // A URL without a hash loads the page again, on the list.
              onClick={() => location.replace(location.pathname + location.search)}
            >
              Back to the list
            </button>
          </div>
        </div>
      </div>
    );
  }
}
