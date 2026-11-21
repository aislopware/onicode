import { StackActions } from "@react-navigation/native";

type ThreadRef = { readonly environmentId: string; readonly threadId: string };

/**
 * Leaves a page opened from a thread. Replacing it while that thread is still below would stack
 * the thread on itself, so Back would lead to the same chat.
 */
export function returnToThreadAction(
  routes: ReadonlyArray<{ readonly name: string; readonly params?: object }> | undefined,
  thread: ThreadRef,
) {
  const below = routes?.findLast((route) => route.name === "Thread")?.params as
    | Partial<ThreadRef>
    | undefined;
  return below?.environmentId === thread.environmentId && below.threadId === thread.threadId
    ? StackActions.popTo("Thread", thread, { merge: true })
    : StackActions.replace("Thread", thread);
}
