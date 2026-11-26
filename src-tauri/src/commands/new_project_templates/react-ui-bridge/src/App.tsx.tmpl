import { useEffect, useState } from "react";
import { State } from "@qontinui/ui-bridge";
import { Button, RecordListPage, TextInput } from "@qontinui/design-kit";

interface Item {
  id: string;
  name: string;
  status: string;
}

const COLUMNS = [
  { key: "name", header: "Name", render: (row: Item) => row.name },
  { key: "status", header: "Status", render: (row: Item) => row.status },
];

/**
 * Replace this loader with the app's real data source. Every state of the page
 * (loading, error, empty, filtered-empty, ready) is already handled below, so
 * the page keeps handling them when the loader changes.
 */
async function loadItems(): Promise<Item[]> {
  return [];
}

export default function App() {
  const [items, setItems] = useState<Item[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [failed, setFailed] = useState(false);
  const [filter, setFilter] = useState("");

  useEffect(() => {
    let cancelled = false;
    loadItems()
      .then((rows) => {
        if (!cancelled) setItems(rows);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      })
      .finally(() => {
        if (!cancelled) setIsLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const rows = items.filter((item) => item.name.toLowerCase().includes(filter.toLowerCase()));

  return (
    <State
      id="home"
      name="Home"
      metadata={{ description: "Starter home page for {{name}}" }}
    >
      <RecordListPage
        title="{{name}}"
        columns={COLUMNS}
        rows={rows}
        rowKey={(row) => row.id}
        isLoading={isLoading}
        isFiltered={filter !== ""}
        failed={failed}
        empty="No items yet. An item is one thing this app tracks; add the first one."
        loading="Loading items."
        error="Items could not be loaded. Check the connection and retry."
        filteredEmpty="No items match this filter. Clear the filter to see all items."
        actions={
          <>
            <TextInput
              aria-label="Filter items"
              placeholder="Filter items"
              value={filter}
              onChange={(event) => setFilter(event.target.value)}
            />
            <Button variant="primary" onClick={() => setFilter("")}>
              Clear filter
            </Button>
          </>
        }
      />
    </State>
  );
}
