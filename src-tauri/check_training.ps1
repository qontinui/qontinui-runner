$dbPath = "C:/Users/Joshua/AppData/Roaming/com.qontinui.runner/runner.db"
Add-Type -Path "C:\Users\jspin\Documents\qontinui_parent\qontinui-runner\src-tauri\tools\System.Data.SQLite.dll" -ErrorAction SilentlyContinue

$conn = New-Object System.Data.SQLite.SQLiteConnection("Data Source=$dbPath;Read Only=True")
$conn.Open()

Write-Host "=== GENERATION PIPELINE ARTIFACTS ===" -ForegroundColor Cyan
$cmd = $conn.CreateCommand()
$cmd.CommandText = "SELECT id, substr(description,1,80) as desc_short, success, builder_prompt IS NOT NULL as has_builder, specification_criteria IS NOT NULL as has_spec, created_at FROM generation_pipeline_artifacts ORDER BY created_at DESC LIMIT 10"
$reader = $cmd.ExecuteReader()
while ($reader.Read()) {
    $id = $reader["id"].ToString().Substring(0, [Math]::Min(8, $reader["id"].ToString().Length))
    Write-Host "$id | $($reader['desc_short']) | success=$($reader['success']) | has_builder=$($reader['has_builder']) | has_spec=$($reader['has_spec']) | $($reader['created_at'])"
}
$reader.Close()

Write-Host "`n=== REFLECTION FIXES ===" -ForegroundColor Cyan
$cmd2 = $conn.CreateCommand()
$cmd2.CommandText = "SELECT substr(id,1,8) as id_short, source_agent, fix_type, effectiveness, status, substr(fix_description,1,100) as desc_short FROM reflection_fixes ORDER BY created_at DESC LIMIT 20"
$reader2 = $cmd2.ExecuteReader()
while ($reader2.Read()) {
    Write-Host "$($reader2['id_short']) | agent=$($reader2['source_agent']) | type=$($reader2['fix_type']) | eff=$($reader2['effectiveness']) | status=$($reader2['status']) | $($reader2['desc_short'])"
}
$reader2.Close()

Write-Host "`n=== WORKFLOW GENERATION FEEDBACK ===" -ForegroundColor Cyan
$cmd3 = $conn.CreateCommand()
$cmd3.CommandText = "SELECT * FROM workflow_generation_feedback ORDER BY created_at DESC LIMIT 10"
try {
    $reader3 = $cmd3.ExecuteReader()
    $hasRows = $false
    while ($reader3.Read()) {
        $hasRows = $true
        for ($i = 0; $i -lt $reader3.FieldCount; $i++) {
            Write-Host "  $($reader3.GetName($i)): $($reader3[$i])"
        }
        Write-Host "---"
    }
    if (-not $hasRows) { Write-Host "(no feedback rows)" }
    $reader3.Close()
} catch {
    Write-Host "(table may not exist: $_)"
}

Write-Host "`n=== TRAINING DATA EXPORT CHECK ===" -ForegroundColor Cyan
$cmd4 = $conn.CreateCommand()
$cmd4.CommandText = @"
SELECT COUNT(*) as total,
       SUM(CASE WHEN builder_prompt IS NOT NULL AND builder_prompt != '' THEN 1 ELSE 0 END) as with_builder_prompt,
       SUM(CASE WHEN specification_criteria IS NOT NULL AND specification_criteria != 'null' THEN 1 ELSE 0 END) as with_spec_criteria,
       SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) as successful
FROM generation_pipeline_artifacts
"@
$reader4 = $cmd4.ExecuteReader()
if ($reader4.Read()) {
    Write-Host "Total artifacts: $($reader4['total'])"
    Write-Host "With builder prompt: $($reader4['with_builder_prompt'])"
    Write-Host "With spec criteria: $($reader4['with_spec_criteria'])"
    Write-Host "Successful: $($reader4['successful'])"
}
$reader4.Close()

Write-Host "`n=== GENERATION RULES ===" -ForegroundColor Cyan
$cmd5 = $conn.CreateCommand()
$cmd5.CommandText = "SELECT substr(id,1,8) as id_short, agent, section, substr(content,1,100) as content_short, source, created_at FROM generation_rules ORDER BY created_at DESC LIMIT 10"
try {
    $reader5 = $cmd5.ExecuteReader()
    $hasRows5 = $false
    while ($reader5.Read()) {
        $hasRows5 = $true
        Write-Host "$($reader5['id_short']) | agent=$($reader5['agent']) | section=$($reader5['section']) | src=$($reader5['source']) | $($reader5['content_short'])"
    }
    if (-not $hasRows5) { Write-Host "(no rules)" }
    $reader5.Close()
} catch {
    Write-Host "(table may not exist: $_)"
}

$conn.Close()
