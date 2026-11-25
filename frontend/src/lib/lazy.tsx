import { type ComponentType, lazy } from "react";

/**
 * Wraps React.lazy so preloaded chunks render synchronously without hitting React's 300ms Suspense throttle.
 */
export function lazyComponent<M, P extends object>(
  load: () => Promise<M>,
  pick: (mod: M) => ComponentType<P>,
): ComponentType<P> & { preload: () => Promise<void> } {
  let Loaded: ComponentType<P> | null = null;
  let pending: Promise<{ default: ComponentType<P> }> | null = null;

  const ensure = (): Promise<{ default: ComponentType<P> }> => {
    if (!pending) {
      pending = load().then((mod) => {
        const comp = pick(mod);
        Loaded = comp;
        return { default: comp };
      });
    }
    return pending;
  };

  const LazyInner = lazy(ensure);
  const Wrapper = (props: P) => (Loaded ? <Loaded {...props} /> : <LazyInner {...props} />);
  Wrapper.preload = async () => {
    await ensure();
  };
  return Wrapper;
}
