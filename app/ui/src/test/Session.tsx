import { useCallback, useEffect, useRef, type ReactNode } from "react";
import type { ProjectView } from "../api/project";
import { ProjectContext } from "../project/context";
import { useSession } from "../project/useSession";

/** The real project session (over whatever `invoke` the test mocks), rendering its children
 * once the first view has arrived. */
export function Session({
  onError,
  children,
}: {
  onError?: (message: string) => void;
  children: (view: ProjectView) => ReactNode;
}) {
  const report = useRef(onError);
  useEffect(() => {
    report.current = onError;
  }, [onError]);
  const session = useSession(useCallback((message: string) => report.current?.(message), []));
  return (
    <ProjectContext.Provider value={session.context}>
      {session.view && children(session.view)}
    </ProjectContext.Provider>
  );
}
