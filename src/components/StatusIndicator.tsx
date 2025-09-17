import React, { useState, useEffect } from 'react';
import { AlertCircle, CheckCircle, Info, AlertTriangle, X, Wifi, WifiOff } from 'lucide-react';
import { listen } from '@tauri-apps/api/event';

interface ErrorEvent {
  title: string;
  message: string;
  details?: string;
  error_code: string;
  severity: 'info' | 'warning' | 'error' | 'critical';
  recoverable: boolean;
  suggested_action?: string;
}

interface StatusIndicatorProps {
  pythonStatus: 'stopped' | 'running';
  configLoaded: boolean;
  executionActive: boolean;
}

const StatusIndicator: React.FC<StatusIndicatorProps> = ({
  pythonStatus,
  configLoaded,
  executionActive
}) => {
  const [error, setError] = useState<ErrorEvent | null>(null);
  const [connectionStatus, setConnectionStatus] = useState<'connected' | 'disconnected' | 'connecting'>('disconnected');
  const [showError, setShowError] = useState(false);
  const [isBeta, setIsBeta] = useState(true);

  useEffect(() => {
    const unlistenError = listen<ErrorEvent>('error', (event) => {
      setError(event.payload);
      setShowError(true);

      // Auto-hide info messages after 5 seconds
      if (event.payload.severity === 'info') {
        setTimeout(() => setShowError(false), 5000);
      }
    });

    const unlistenConnection = listen<{ status: 'connected' | 'disconnected' | 'connecting' }>('connection-status', (event) => {
      setConnectionStatus(event.payload.status);
    });

    return () => {
      unlistenError.then(fn => fn());
      unlistenConnection.then(fn => fn());
    };
  }, []);

  const getSeverityColor = (severity: string) => {
    switch (severity) {
      case 'info': return 'bg-blue-100 text-blue-800 border-blue-200';
      case 'warning': return 'bg-yellow-100 text-yellow-800 border-yellow-200';
      case 'error': return 'bg-red-100 text-red-800 border-red-200';
      case 'critical': return 'bg-red-200 text-red-900 border-red-300';
      default: return 'bg-gray-100 text-gray-800 border-gray-200';
    }
  };

  const getSeverityIcon = (severity: string) => {
    switch (severity) {
      case 'info': return <Info className="w-5 h-5" />;
      case 'warning': return <AlertTriangle className="w-5 h-5" />;
      case 'error': return <AlertCircle className="w-5 h-5" />;
      case 'critical': return <AlertCircle className="w-5 h-5" />;
      default: return <Info className="w-5 h-5" />;
    }
  };

  return (
    <div className="relative">
      {/* Beta Badge */}
      {isBeta && (
        <div className="fixed top-4 right-4 z-50">
          <span className="px-3 py-1 text-xs font-semibold bg-gradient-to-r from-purple-500 to-indigo-500 text-white rounded-full shadow-lg">
            BETA
          </span>
        </div>
      )}

      {/* Status Bar */}
      <div className="flex items-center gap-4 px-4 py-2 bg-gray-50 border-b border-gray-200">
        <div className="flex items-center gap-2">
          <div className={`w-2 h-2 rounded-full ${pythonStatus === 'running' ? 'bg-green-500' : 'bg-gray-400'}`} />
          <span className="text-sm text-gray-600">Executor: {pythonStatus}</span>
        </div>

        <div className="flex items-center gap-2">
          <div className={`w-2 h-2 rounded-full ${configLoaded ? 'bg-green-500' : 'bg-gray-400'}`} />
          <span className="text-sm text-gray-600">Config: {configLoaded ? 'Loaded' : 'Not Loaded'}</span>
        </div>

        <div className="flex items-center gap-2">
          <div className={`w-2 h-2 rounded-full ${executionActive ? 'bg-blue-500 animate-pulse' : 'bg-gray-400'}`} />
          <span className="text-sm text-gray-600">Execution: {executionActive ? 'Active' : 'Inactive'}</span>
        </div>

        <div className="flex items-center gap-2 ml-auto">
          {connectionStatus === 'connected' ? (
            <>
              <Wifi className="w-4 h-4 text-green-500" />
              <span className="text-sm text-gray-600">Connected to Qontinui</span>
            </>
          ) : connectionStatus === 'connecting' ? (
            <>
              <Wifi className="w-4 h-4 text-yellow-500 animate-pulse" />
              <span className="text-sm text-gray-600">Connecting...</span>
            </>
          ) : (
            <>
              <WifiOff className="w-4 h-4 text-gray-400" />
              <span className="text-sm text-gray-600">Offline Mode</span>
            </>
          )}
        </div>
      </div>

      {/* Error Display */}
      {showError && error && (
        <div className={`fixed top-20 right-4 max-w-md z-40 p-4 rounded-lg shadow-lg border ${getSeverityColor(error.severity)}`}>
          <div className="flex items-start gap-3">
            <div className="flex-shrink-0 mt-0.5">
              {getSeverityIcon(error.severity)}
            </div>
            <div className="flex-1">
              <div className="flex justify-between items-start">
                <h3 className="font-semibold">{error.title}</h3>
                <button
                  onClick={() => setShowError(false)}
                  className="ml-2 p-1 hover:bg-black/10 rounded"
                >
                  <X className="w-4 h-4" />
                </button>
              </div>
              <p className="mt-1 text-sm">{error.message}</p>
              {error.details && (
                <details className="mt-2">
                  <summary className="text-xs cursor-pointer hover:underline">Technical Details</summary>
                  <pre className="mt-1 text-xs bg-black/10 p-2 rounded overflow-x-auto">{error.details}</pre>
                </details>
              )}
              {error.suggested_action && (
                <p className="mt-2 text-sm font-medium">
                  💡 {error.suggested_action}
                </p>
              )}
              <p className="mt-2 text-xs opacity-60">Error Code: {error.error_code}</p>
            </div>
          </div>
        </div>
      )}
    </div>
  );
};

export default StatusIndicator;