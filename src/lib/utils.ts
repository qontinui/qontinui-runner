import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** Extract a message string from an unknown caught error. */
export function getErrorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
