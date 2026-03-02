"""Setup script for the 'Fix Data Pipeline' demo workflow.

Creates a data processing pipeline with bugs in the transformation logic.
The pipeline reads CSV data, processes it, and outputs results.
The AI must fix the processing bugs so the output matches expectations.
"""
import pathlib
import json

WORKSPACE = pathlib.Path(r"C:/temp/qontinui-demo/data-pipeline")
WORKSPACE.mkdir(parents=True, exist_ok=True)

# Input CSV data
(WORKSPACE / "sales_data.csv").write_text("""\
date,product,quantity,unit_price,region
2024-01-15,Widget A,10,29.99,North
2024-01-15,Widget B,5,49.99,South
2024-01-16,Widget A,3,29.99,East
2024-01-16,Widget C,8,19.99,North
2024-01-17,Widget B,12,49.99,West
2024-01-17,Widget A,7,29.99,South
2024-01-18,Widget C,15,19.99,East
2024-01-18,Widget B,2,49.99,North
""", encoding='utf-8')

# Data pipeline with bugs
(WORKSPACE / "pipeline.py").write_text('''\
"""Sales data processing pipeline."""
import csv
import json
from collections import defaultdict


def read_csv(filepath):
    """Read CSV file and return list of dicts."""
    with open(filepath, 'r') as f:
        reader = csv.DictReader(f)
        return list(reader)


def calculate_revenue(rows):
    """Calculate total revenue for each row. Returns rows with 'revenue' field added."""
    result = []
    for row in rows:
        row = dict(row)
        # BUG: quantity and unit_price are strings from CSV, need to convert
        row['revenue'] = row['quantity'] + row['unit_price']  # BUG: string concatenation, not multiplication
        result.append(row)
    return result


def aggregate_by_product(rows):
    """Aggregate total quantity and revenue by product."""
    agg = defaultdict(lambda: {'total_quantity': 0, 'total_revenue': 0.0})
    for row in rows:
        product = row['product']
        agg[product]['total_quantity'] += int(row['quantity'])
        # BUG: revenue might still be a string if calculate_revenue has issues
        agg[product]['total_revenue'] += float(row['revenue'])
    return dict(agg)


def aggregate_by_region(rows):
    """Aggregate total revenue by region."""
    agg = defaultdict(float)
    for row in rows:
        agg[row['region']] += float(row['revenue'])
    return dict(agg)


def find_top_product(product_agg):
    """Find product with highest total revenue."""
    # BUG: finds LOWEST instead of highest
    return min(product_agg.items(), key=lambda x: x[1]['total_revenue'])[0]


def generate_report(rows, product_agg, region_agg, top_product):
    """Generate JSON report."""
    return {
        'total_rows': len(rows),
        'total_revenue': sum(float(r['revenue']) for r in rows),
        'by_product': product_agg,
        'by_region': region_agg,
        'top_product': top_product,
    }


def main():
    rows = read_csv('sales_data.csv')
    rows = calculate_revenue(rows)
    product_agg = aggregate_by_product(rows)
    region_agg = aggregate_by_region(rows)
    top_product = find_top_product(product_agg)
    report = generate_report(rows, product_agg, region_agg, top_product)

    with open('report.json', 'w') as f:
        json.dump(report, f, indent=2)

    print(json.dumps(report, indent=2))
    return report


if __name__ == '__main__':
    main()
''', encoding='utf-8')

# Validation script (correct expected output)
(WORKSPACE / "validate_pipeline.py").write_text('''\
"""Validate the pipeline output against expected values."""
import sys
import json
import os

# Run the pipeline first
print("Running pipeline...")
exit_code = os.system('python pipeline.py > pipeline_output.txt 2>&1')
if exit_code != 0:
    print("FAIL: Pipeline crashed with non-zero exit code")
    with open('pipeline_output.txt', 'r') as f:
        print(f.read())
    sys.exit(1)

# Load the report
try:
    with open('report.json', 'r') as f:
        report = json.load(f)
except (FileNotFoundError, json.JSONDecodeError) as e:
    print(f"FAIL: Could not read report.json: {e}")
    sys.exit(1)

failures = []


def check(actual, expected, name):
    if abs(actual - expected) > 0.01 if isinstance(expected, float) else actual != expected:
        failures.append(f"FAIL: {name} - expected {expected}, got {actual}")
    else:
        print(f"PASS: {name}")


# Expected values calculated manually:
# Widget A: (10*29.99) + (3*29.99) + (7*29.99) = 299.9 + 89.97 + 209.93 = 599.80
# Widget B: (5*49.99) + (12*49.99) + (2*49.99) = 249.95 + 599.88 + 99.98 = 949.81
# Widget C: (8*19.99) + (15*19.99) = 159.92 + 299.85 = 459.77

check(report['total_rows'], 8, 'total_rows')
check(report['total_revenue'], 2009.38, 'total_revenue')
check(report['top_product'], 'Widget B', 'top_product is Widget B')

# Check product aggregations
check(report['by_product']['Widget A']['total_quantity'], 20, 'Widget A quantity')
check(report['by_product']['Widget A']['total_revenue'], 599.80, 'Widget A revenue')
check(report['by_product']['Widget B']['total_quantity'], 19, 'Widget B quantity')
check(report['by_product']['Widget B']['total_revenue'], 949.81, 'Widget B revenue')
check(report['by_product']['Widget C']['total_quantity'], 23, 'Widget C quantity')
check(report['by_product']['Widget C']['total_revenue'], 459.77, 'Widget C revenue')

# Check region aggregations
# North: (10*29.99) + (8*19.99) + (2*49.99) = 299.9 + 159.92 + 99.98 = 559.80
# South: (5*49.99) + (7*29.99) = 249.95 + 209.93 = 459.88
# East: (3*29.99) + (15*19.99) = 89.97 + 299.85 = 389.82
# West: (12*49.99) = 599.88
check(report['by_region']['North'], 559.80, 'North revenue')
check(report['by_region']['South'], 459.88, 'South revenue')
check(report['by_region']['East'], 389.82, 'East revenue')
check(report['by_region']['West'], 599.88, 'West revenue')

if failures:
    print(f"\\n{len(failures)} check(s) FAILED:")
    for f in failures:
        print(f"  {f}")
    sys.exit(1)
else:
    print(f"\\nAll checks passed! Pipeline output is correct.")
    sys.exit(0)
''', encoding='utf-8')

print(f"Demo workspace created at: {WORKSPACE}")
print("Files: sales_data.csv, pipeline.py (with 3 bugs), validate_pipeline.py")
