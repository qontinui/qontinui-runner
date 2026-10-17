import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import type { FleetSessionsResponse } from "./useFleetSessions";
import { walkFinishedFleetSessions, type FinishedFleetRead } from "./remoteSessionEndView";

/**
 * The fleet's FINISHED sessions, as one server-side set: `GET
 * /coord/sessions/fleet?session_status=finished` walked to the end (bounded),
 * independent of whatever filters the picker's own list carries — "Close all
 * finished" must not mean "close the finished ones that happen to be on the
 * page you loaded".
 *
 * Every row is re-checked against its own `sessionStatus`
 * (`walkFinishedFleetSessions`), because a coord predating the filter ignores
 * the parameter and serves every session.
 *
 * The read is `unavailable` — never a candidate list — when coord does not
 * name this runner's device (`callerDeviceId` null) or cannot read the device
 * identity columns: the bulk end must never be able to reach a LOCAL session,
 * and without both it cannot tell one from a remote one. An `ok` read carries
 * the `callerDeviceId` that `closeAllFinishedCandidates` excludes by.
 */
export function useFinishedFleetSessions(): {
  read: FinishedFleetRead;
  /** Re-walk, and resolve with the fresh read (also stored as `read`). */
  refresh: () => Promise<FinishedFleetRead>;
} {
  const [read, setRead] = useState<FinishedFleetRead>({ kind: "loading" });
  const generation = useRef(0);

  /** One walk. Sets state only once it settles, and only if still the newest. */
  const load = useCallback(async (): Promise<FinishedFleetRead> => {
    const mine = ++generation.current;
    const result = await walkFinishedFleetSessions(({ sessionStatus, limit, cursor }) =>
      invoke<FleetSessionsResponse>("fleet_sessions_list", {
        args: { deviceId: null, state: null, sessionStatus, includeClosed: false, limit, cursor },
      }),
    );
    if (generation.current === mine) setRead(result);
    return result;
  }, []);

  // The initial state is already `loading`, so the mount read needs no reset.
  useEffect(() => {
    void load();
  }, [load]);

  const refresh = useCallback(async () => {
    setRead({ kind: "loading" });
    return load();
  }, [load]);

  return { read, refresh };
}
