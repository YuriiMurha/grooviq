import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// Vitest has no globals here, so Testing Library cannot unmount between tests by itself.
afterEach(cleanup);
