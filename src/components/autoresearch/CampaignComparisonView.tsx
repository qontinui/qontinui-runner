/**
 * CampaignComparisonView — Side-by-side comparison of two autoresearch campaigns.
 */

interface CampaignSummary {
  id: string;
  name: string;
  status: string;
  experiment_count: number;
  accepted_count: number;
  config_json: string;
  created_at: string;
}

interface CampaignComparison {
  campaign_a: CampaignSummary;
  campaign_b: CampaignSummary;
  pass_rate_delta: number;
  duration_delta: number;
  accepted_rate_delta: number;
}

function DeltaIndicator({ delta, higherIsBetter }: { delta: number; higherIsBetter: boolean }) {
  if (Math.abs(delta) < 0.001) {
    return <span className="text-zinc-500 text-xs">--</span>;
  }
  const isPositive = delta > 0;
  const isImproved = higherIsBetter ? isPositive : !isPositive;
  const arrow = isPositive ? "\u2191" : "\u2193";
  const color = isImproved ? "text-green-400" : "text-red-400";

  return (
    <span className={`text-xs font-medium ${color}`}>
      {arrow} {Math.abs(delta).toFixed(2)}
    </span>
  );
}

interface Props {
  comparison: CampaignComparison;
  onClose: () => void;
}

export function CampaignComparisonView({ comparison, onClose }: Props) {
  const { campaign_a, campaign_b } = comparison;

  return (
    <div className="bg-zinc-900 border border-zinc-700 rounded-lg p-4">
      <div className="flex items-center justify-between mb-4">
        <div className="text-sm text-zinc-300 font-medium">Campaign Comparison</div>
        <button onClick={onClose} className="text-xs text-zinc-500 hover:text-zinc-300 px-2 py-1">
          Close
        </button>
      </div>

      <table className="w-full text-xs">
        <thead>
          <tr className="text-zinc-500 border-b border-zinc-800">
            <th className="text-left py-2 px-2">Metric</th>
            <th className="text-right py-2 px-2 max-w-[200px] truncate">{campaign_a.name}</th>
            <th className="text-right py-2 px-2 max-w-[200px] truncate">{campaign_b.name}</th>
            <th className="text-right py-2 px-2">Delta</th>
          </tr>
        </thead>
        <tbody>
          <tr className="border-b border-zinc-800/50">
            <td className="py-2 px-2 text-zinc-400">Date</td>
            <td className="py-2 px-2 text-right text-zinc-300">
              {new Date(campaign_a.created_at).toLocaleDateString()}
            </td>
            <td className="py-2 px-2 text-right text-zinc-300">
              {new Date(campaign_b.created_at).toLocaleDateString()}
            </td>
            <td className="py-2 px-2 text-right text-zinc-500">--</td>
          </tr>
          <tr className="border-b border-zinc-800/50">
            <td className="py-2 px-2 text-zinc-400">Experiments</td>
            <td className="py-2 px-2 text-right text-zinc-300">{campaign_a.experiment_count}</td>
            <td className="py-2 px-2 text-right text-zinc-300">{campaign_b.experiment_count}</td>
            <td className="py-2 px-2 text-right text-zinc-500">
              {campaign_b.experiment_count - campaign_a.experiment_count}
            </td>
          </tr>
          <tr className="border-b border-zinc-800/50">
            <td className="py-2 px-2 text-zinc-400">Accepted</td>
            <td className="py-2 px-2 text-right text-zinc-300">{campaign_a.accepted_count}</td>
            <td className="py-2 px-2 text-right text-zinc-300">{campaign_b.accepted_count}</td>
            <td className="py-2 px-2 text-right">
              <DeltaIndicator delta={comparison.accepted_rate_delta * 100} higherIsBetter={true} />
            </td>
          </tr>
          <tr className="border-b border-zinc-800/50">
            <td className="py-2 px-2 text-zinc-400">Pass Rate</td>
            <td className="py-2 px-2 text-right text-zinc-300">--</td>
            <td className="py-2 px-2 text-right text-zinc-300">--</td>
            <td className="py-2 px-2 text-right">
              <DeltaIndicator delta={comparison.pass_rate_delta} higherIsBetter={true} />
            </td>
          </tr>
          <tr className="border-b border-zinc-800/50">
            <td className="py-2 px-2 text-zinc-400">Avg Duration</td>
            <td className="py-2 px-2 text-right text-zinc-300">--</td>
            <td className="py-2 px-2 text-right text-zinc-300">--</td>
            <td className="py-2 px-2 text-right">
              <DeltaIndicator delta={comparison.duration_delta} higherIsBetter={false} />
            </td>
          </tr>
          <tr>
            <td className="py-2 px-2 text-zinc-400">Status</td>
            <td className="py-2 px-2 text-right text-zinc-300">{campaign_a.status}</td>
            <td className="py-2 px-2 text-right text-zinc-300">{campaign_b.status}</td>
            <td className="py-2 px-2 text-right text-zinc-500">--</td>
          </tr>
        </tbody>
      </table>
    </div>
  );
}
