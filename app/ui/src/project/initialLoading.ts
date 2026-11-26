const NOTICE_DELAY_MS = 200;

export function scheduleInitialLoading(show: () => void): () => void {
  const timer = setTimeout(show, NOTICE_DELAY_MS);
  return () => clearTimeout(timer);
}
