import { useContext } from "react";
import { ZoneMetadataContext, type ZoneMetadataContextValue } from "./ZoneMetadataContext";

export function useZoneMetadata(): ZoneMetadataContextValue {
  const ctx = useContext(ZoneMetadataContext);
  if (!ctx) {
    throw new Error("useZoneMetadata must be used within a ZoneMetadataProvider");
  }
  return ctx;
}
