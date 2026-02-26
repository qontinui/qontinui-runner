/**
 * useLibraryBuilder
 *
 * Builder page state management hook that combines useRunnerCrud with
 * selection, form state, search filtering, and CRUD orchestration.
 * Used by library builder pages that follow the split-panel pattern
 * (item list on left, editor form on right).
 */

import { useState, useCallback, useMemo } from "react";
import { useRunnerCrud, type UseRunnerCrudReturn } from "./useRunnerCrud";

export interface UseLibraryBuilderOptions<T extends { id: string }> {
  resourcePath: string;
  defaultFormState: Partial<T>;
  toFormState: (item: T) => Partial<T>;
  toRequest: (form: Partial<T>) => unknown;
}

export interface UseLibraryBuilderReturn<T extends { id: string }> {
  // From useRunnerCrud
  items: T[];
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;

  // Selection
  selectedItem: T | null;
  selectItem: (item: T | null) => void;

  // Form state
  formState: Partial<T>;
  setFormState: React.Dispatch<React.SetStateAction<Partial<T>>>;
  isDirty: boolean;

  // CRUD orchestration
  isCreating: boolean;
  startCreate: () => void;
  cancelEdit: () => void;
  save: () => Promise<void>;
  deleteSelected: () => Promise<void>;
  runSelected: (action?: string) => Promise<unknown>;

  // Search
  searchQuery: string;
  setSearchQuery: (query: string) => void;
  filteredItems: T[];
}

/**
 * Builder page state management hook.
 */
export function useLibraryBuilder<T extends { id: string }>(
  options: UseLibraryBuilderOptions<T>,
): UseLibraryBuilderReturn<T> {
  const { resourcePath, defaultFormState, toFormState, toRequest } = options;

  const crud: UseRunnerCrudReturn<T> = useRunnerCrud<T>({ resourcePath });

  const [selectedItem, setSelectedItem] = useState<T | null>(null);
  const [formState, setFormState] = useState<Partial<T>>(defaultFormState);
  const [originalFormState, setOriginalFormState] = useState<Partial<T>>(defaultFormState);
  const [isCreating, setIsCreating] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");

  const isDirty = useMemo(() => {
    return JSON.stringify(formState) !== JSON.stringify(originalFormState);
  }, [formState, originalFormState]);

  const filteredItems = useMemo(() => {
    if (!searchQuery.trim()) {
      return crud.items;
    }
    const query = searchQuery.toLowerCase();
    return crud.items.filter((item) => {
      const record = item as Record<string, unknown>;
      const name = typeof record.name === "string" ? record.name : "";
      const label = typeof record.label === "string" ? record.label : "";
      return name.toLowerCase().includes(query) || label.toLowerCase().includes(query);
    });
  }, [crud.items, searchQuery]);

  const selectItem = useCallback(
    (item: T | null) => {
      setSelectedItem(item);
      setIsCreating(false);
      if (item) {
        const form = toFormState(item);
        setFormState(form);
        setOriginalFormState(form);
      } else {
        setFormState(defaultFormState);
        setOriginalFormState(defaultFormState);
      }
    },
    [toFormState, defaultFormState],
  );

  const startCreate = useCallback(() => {
    setSelectedItem(null);
    setIsCreating(true);
    setFormState(defaultFormState);
    setOriginalFormState(defaultFormState);
  }, [defaultFormState]);

  const cancelEdit = useCallback(() => {
    if (isCreating) {
      setIsCreating(false);
      setFormState(defaultFormState);
      setOriginalFormState(defaultFormState);
    } else if (selectedItem) {
      const form = toFormState(selectedItem);
      setFormState(form);
      setOriginalFormState(form);
    }
  }, [isCreating, selectedItem, toFormState, defaultFormState]);

  const save = useCallback(async () => {
    const requestBody = toRequest(formState);

    if (isCreating) {
      const created = await crud.create(requestBody as Partial<T>);
      if (created) {
        setIsCreating(false);
        setSelectedItem(created);
        const form = toFormState(created);
        setFormState(form);
        setOriginalFormState(form);
      }
    } else if (selectedItem) {
      const updated = await crud.update(selectedItem.id, requestBody as Partial<T>);
      if (updated) {
        setSelectedItem(updated);
        const form = toFormState(updated);
        setFormState(form);
        setOriginalFormState(form);
      }
    }
  }, [isCreating, selectedItem, formState, toRequest, toFormState, crud]);

  const deleteSelected = useCallback(async () => {
    if (!selectedItem) return;

    const success = await crud.remove(selectedItem.id);
    if (success) {
      setSelectedItem(null);
      setIsCreating(false);
      setFormState(defaultFormState);
      setOriginalFormState(defaultFormState);
    }
  }, [selectedItem, crud, defaultFormState]);

  const runSelected = useCallback(
    async (action = "run"): Promise<unknown> => {
      if (!selectedItem) return null;
      return crud.runAction(selectedItem.id, action);
    },
    [selectedItem, crud],
  );

  return {
    // From useRunnerCrud
    items: crud.items,
    loading: crud.loading,
    error: crud.error,
    refresh: crud.refresh,

    // Selection
    selectedItem,
    selectItem,

    // Form state
    formState,
    setFormState,
    isDirty,

    // CRUD orchestration
    isCreating,
    startCreate,
    cancelEdit,
    save,
    deleteSelected,
    runSelected,

    // Search
    searchQuery,
    setSearchQuery,
    filteredItems,
  };
}
