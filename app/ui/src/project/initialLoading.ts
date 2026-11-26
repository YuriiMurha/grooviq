const NOTICE_DELAY_MS = 200;

export type InitialLoadState = "pending" | "slow" | "settled";

export function trackInitialLoad(request: Promise<unknown>, onState: (state: InitialLoadState) => void): () => void {
  let active = true;
  const timer = setTimeout(() => onState("slow"), NOTICE_DELAY_MS);
  const finish = () => {
    if (!active) return;
    clearTimeout(timer);
    onState("settled");
  };
  void request.then(finish, finish);

  return () => {
    active = false;
    clearTimeout(timer);
  };
}
