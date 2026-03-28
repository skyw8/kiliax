import React from "react";

type Props = {
  children: React.ReactNode;
};

type State = {
  error: Error | null;
};

export class ErrorBoundary extends React.Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    // eslint-disable-next-line no-console
    console.error("render error", error, info);
  }

  render() {
    if (!this.state.error) {
      return this.props.children;
    }

    return (
      <div className="h-dvh w-full bg-white text-zinc-900">
        <div className="flex h-full items-center justify-center p-6">
          <div className="w-full max-w-xl rounded-lg border border-zinc-200 bg-white p-6">
            <div className="text-base font-semibold">Something went wrong</div>
            <div className="mt-2 text-sm text-zinc-600">
              The UI crashed while rendering.
            </div>
            <pre className="mt-4 max-h-[40vh] overflow-auto rounded-md bg-zinc-50 p-3 text-xs text-zinc-800">
              {this.state.error.message}
            </pre>
            <div className="mt-4 flex justify-end gap-2">
              <button
                className="rounded-md border border-zinc-200 bg-white px-3 py-2 text-sm hover:bg-zinc-50"
                onClick={() => window.location.reload()}
              >
                Reload
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }
}

