import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import './App.css';

interface ExecutorEvent {
  event: string;
  timestamp: number;
  data: any;
}

interface CommandResponse {
  success: boolean;
  message?: string;
  data?: any;
}

function App() {
  const [configPath, setConfigPath] = useState<string>('');
  const [configLoaded, setConfigLoaded] = useState(false);
  const [pythonRunning, setPythonRunning] = useState(false);
  const [executionRunning, setExecutionRunning] = useState(false);
  const [logs, setLogs] = useState<string[]>([]);
  const [currentState, setCurrentState] = useState<string>('');
  const [, setEvents] = useState<ExecutorEvent[]>([]);
  const [executorType, setExecutorType] = useState<'simple' | 'qontinui'>('simple');
  const [processes, setProcesses] = useState<any[]>([]);
  const [selectedProcess, setSelectedProcess] = useState<string>('');
  const [executionMode, setExecutionMode] = useState<'process' | 'state_machine'>('process');

  useEffect(() => {
    // Listen for executor events
    const unlisten = listen<ExecutorEvent>('executor-event', (event) => {
      const executorEvent = event.payload;
      setEvents(prev => [...prev, executorEvent]);
      
      // Handle specific events
      switch (executorEvent.event) {
        case 'log':
          const logData = executorEvent.data;
          setLogs(prev => [...prev, `[${logData.level}] ${logData.message}`]);
          break;
        case 'state_changed':
          setCurrentState(executorEvent.data.to_state || executorEvent.data.state_id);
          break;
        case 'execution_started':
          setExecutionRunning(true);
          break;
        case 'execution_completed':
          setExecutionRunning(false);
          break;
        case 'error':
          setLogs(prev => [...prev, `[ERROR] ${executorEvent.data.message}`]);
          break;
      }
    });

    // Check initial status
    checkStatus();

    return () => {
      unlisten.then(f => f());
    };
  }, []);

  const checkStatus = async () => {
    try {
      const response = await invoke<CommandResponse>('get_executor_status');
      if (response.data) {
        setPythonRunning(response.data.python_running);
        setConfigLoaded(response.data.config_loaded);
      }
    } catch (error) {
      console.error('Failed to get status:', error);
    }
  };

  const selectConfigFile = async () => {
    const selected = await open({
      filters: [{
        name: 'JSON Configuration',
        extensions: ['json']
      }]
    });

    if (selected && typeof selected === 'string') {
      setConfigPath(selected);
      await loadConfiguration(selected);
    }
  };

  const loadConfiguration = async (path: string) => {
    try {
      const response = await invoke<CommandResponse>('load_configuration', { path });
      if (response.success) {
        setConfigLoaded(true);
        setLogs(prev => [...prev, `Configuration loaded: ${response.message}`]);
        
        // Extract processes from the loaded configuration
        if (response.data && response.data.processes) {
          setProcesses(response.data.processes);
          if (response.data.processes.length > 0) {
            setSelectedProcess(response.data.processes[0].id);
          }
        }
      } else {
        setLogs(prev => [...prev, `Failed to load configuration: ${response.message}`]);
      }
    } catch (error) {
      setLogs(prev => [...prev, `Error loading configuration: ${error}`]);
    }
  };

  const startPythonExecutor = async () => {
    try {
      const response = await invoke<CommandResponse>('start_python_executor_with_type', {
        executorType: executorType
      });
      if (response.success) {
        setPythonRunning(true);
        setLogs(prev => [...prev, `Python executor started (${executorType} mode)`]);
        
        // Load configuration if available
        if (configPath && configLoaded) {
          await loadConfiguration(configPath);
        }
      } else {
        setLogs(prev => [...prev, `Failed to start Python: ${response.message}`]);
      }
    } catch (error) {
      setLogs(prev => [...prev, `Error starting Python: ${error}`]);
    }
  };

  const stopPythonExecutor = async () => {
    try {
      const response = await invoke<CommandResponse>('stop_python_executor');
      if (response.success) {
        setPythonRunning(false);
        setExecutionRunning(false);
        setLogs(prev => [...prev, 'Python executor stopped']);
      }
    } catch (error) {
      setLogs(prev => [...prev, `Error stopping Python: ${error}`]);
    }
  };

  const startExecution = async () => {
    if (!pythonRunning) {
      setLogs(prev => [...prev, 'Please start Python executor first']);
      return;
    }
    
    if (!configLoaded) {
      setLogs(prev => [...prev, 'Please load a configuration first']);
      return;
    }
    
    if (executionMode === 'process' && !selectedProcess) {
      setLogs(prev => [...prev, 'Please select a process to run']);
      return;
    }

    try {
      const params = executionMode === 'process' 
        ? { mode: 'process', processId: selectedProcess }
        : { mode: 'state_machine' };
      
      const response = await invoke<CommandResponse>('start_execution', params);
      if (response.success) {
        setExecutionRunning(true);
        setLogs(prev => [...prev, `Execution started (${executionMode} mode)`]);
      } else {
        setLogs(prev => [...prev, `Failed to start execution: ${response.message}`]);
      }
    } catch (error) {
      setLogs(prev => [...prev, `Error starting execution: ${error}`]);
    }
  };

  const stopExecution = async () => {
    try {
      const response = await invoke<CommandResponse>('stop_execution');
      if (response.success) {
        setExecutionRunning(false);
        setLogs(prev => [...prev, 'Execution stopped']);
      }
    } catch (error) {
      setLogs(prev => [...prev, `Error stopping execution: ${error}`]);
    }
  };

  const clearLogs = () => {
    setLogs([]);
    setEvents([]);
  };

  return (
    <div className="container">
      <h1>Qontinui Runner</h1>
      
      <div className="status-bar">
        <span className={`status-indicator ${pythonRunning ? 'active' : ''}`}>
          Python: {pythonRunning ? 'Running' : 'Stopped'}
        </span>
        <span className={`status-indicator ${configLoaded ? 'active' : ''}`}>
          Config: {configLoaded ? 'Loaded' : 'Not Loaded'}
        </span>
        <span className={`status-indicator ${executionRunning ? 'active' : ''}`}>
          Execution: {executionRunning ? 'Running' : 'Idle'}
        </span>
        {currentState && (
          <span className="status-indicator">
            State: {currentState}
          </span>
        )}
      </div>

      <div className="control-panel">
        <div className="section">
          <h2>Configuration</h2>
          <button onClick={selectConfigFile}>
            Load Configuration
          </button>
          {configPath && (
            <div className="config-path">
              {configPath}
            </div>
          )}
        </div>

        <div className="section">
          <h2>Python Executor</h2>
          <div style={{ marginBottom: '10px' }}>
            <label style={{ marginRight: '10px' }}>
              <input
                type="radio"
                value="simple"
                checked={executorType === 'simple'}
                onChange={(e) => setExecutorType(e.target.value as 'simple' | 'qontinui')}
                disabled={pythonRunning}
              />
              Simulation Mode
            </label>
            <label>
              <input
                type="radio"
                value="qontinui"
                checked={executorType === 'qontinui'}
                onChange={(e) => setExecutorType(e.target.value as 'simple' | 'qontinui')}
                disabled={pythonRunning}
              />
              Real Automation (Qontinui)
            </label>
          </div>
          <button 
            onClick={startPythonExecutor}
            disabled={pythonRunning}
          >
            Start Python
          </button>
          <button 
            onClick={stopPythonExecutor}
            disabled={!pythonRunning}
          >
            Stop Python
          </button>
        </div>

        <div className="section">
          <h2>Execution Control</h2>
          <div style={{ marginBottom: '10px' }}>
            <label style={{ marginRight: '10px' }}>
              <input
                type="radio"
                value="process"
                checked={executionMode === 'process'}
                onChange={(e) => setExecutionMode(e.target.value as 'process' | 'state_machine')}
                disabled={executionRunning}
              />
              Run Process
            </label>
            <label>
              <input
                type="radio"
                value="state_machine"
                checked={executionMode === 'state_machine'}
                onChange={(e) => setExecutionMode(e.target.value as 'process' | 'state_machine')}
                disabled={executionRunning}
              />
              Run State Machine
            </label>
          </div>
          
          {executionMode === 'process' && processes.length > 0 && (
            <div style={{ marginBottom: '10px' }}>
              <select 
                value={selectedProcess} 
                onChange={(e) => setSelectedProcess(e.target.value)}
                disabled={executionRunning}
                style={{ width: '100%', padding: '5px' }}
              >
                {processes.map((process: any) => (
                  <option key={process.id} value={process.id}>
                    {process.name || process.id}
                  </option>
                ))}
              </select>
            </div>
          )}
          
          <button 
            onClick={startExecution}
            disabled={!pythonRunning || !configLoaded || executionRunning}
          >
            Start Execution
          </button>
          <button 
            onClick={stopExecution}
            disabled={!executionRunning}
          >
            Stop Execution
          </button>
        </div>
      </div>

      <div className="log-section">
        <div className="log-header">
          <h2>Logs</h2>
          <button onClick={clearLogs}>Clear</button>
        </div>
        <div className="log-viewer">
          {logs.map((log, index) => {
            // Determine log level for styling
            let logClass = 'log-entry';
            if (log.includes('[ERROR]') || log.includes('Error')) {
              logClass += ' error';
            } else if (log.includes('[WARNING]') || log.includes('[warning]')) {
              logClass += ' warning';
            } else if (log.includes('[INFO]') || log.includes('[info]')) {
              logClass += ' info';
            } else if (log.includes('[DEBUG]') || log.includes('[debug]')) {
              logClass += ' debug';
            } else if (log.includes('success') || log.includes('completed') || log.includes('Loaded')) {
              logClass += ' success';
            }
            
            // Format timestamp if present
            const timestamp = new Date().toLocaleTimeString();
            
            return (
              <div key={index} className={logClass}>
                <span className="log-timestamp">{timestamp}</span>
                {log}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

export default App;