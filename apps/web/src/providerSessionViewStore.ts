import type { ProviderSessionView } from "@onicode/client-runtime/provider-session-views";
import { create } from "zustand";

interface ProviderSessionViewState {
  readonly open: { readonly threadKey: string; readonly view: ProviderSessionView } | null;
  readonly show: (threadKey: string, view: ProviderSessionView) => void;
  readonly close: () => void;
}

/** One session view at a time; the composer, the details card and the context view each open one. */
export const useProviderSessionViewStore = create<ProviderSessionViewState>((set) => ({
  open: null,
  show: (threadKey, view) => set({ open: { threadKey, view } }),
  close: () => set({ open: null }),
}));
