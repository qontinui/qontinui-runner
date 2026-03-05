$db = "C:/Users/Joshua/AppData/Roaming/com.qontinui.runner/runner.db"
Add-Type -Path "C:/Users/jspin/.nuget/packages/system.data.sqlite.core/1.0.118/lib/netstandard2.1/System.Data.SQLite.dll" -ErrorAction SilentlyContinue

# Try using the runner binary itself
$conn = New-Object System.Data.SQLite.SQLiteConnection("Data Source=$db;Read Only=True")
$conn.Open()
$cmd = $conn.CreateCommand()
$cmd.CommandText = "SELECT id, created_at, substr(artifact_json, 1, 1000) as snippet FROM generation_pipeline_artifacts ORDER BY created_at DESC LIMIT 1"
$reader = $cmd.ExecuteReader()
while ($reader.Read()) {
    Write-Output "ID: $($reader[0])"
    Write-Output "Created: $($reader[1])"
    Write-Output "Snippet: $($reader[2])"
}
$conn.Close()
