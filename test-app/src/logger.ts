import { invoke } from "@tauri-apps/api/core";

// Logger that writes to both console and file
class Logger {
  private buffer: string[] = [];
  private flushInterval: number | null = null;
  private enabled = false;

  init() {
    // We'll enable logging after protocol starts
    // The backend will determine the log file path based on HARBOR_DB_PATH
    this.enabled = true;

    // Flush buffer every 500ms
    this.flushInterval = window.setInterval(() => this.flush(), 500);
  }

  log(...args: any[]) {
    const message = args.map(a =>
      typeof a === 'object' ? JSON.stringify(a) : String(a)
    ).join(' ');

    console.log(...args);

    if (this.enabled) {
      this.buffer.push(message);
    }
  }

  error(...args: any[]) {
    const message = `ERROR: ${args.map(a =>
      typeof a === 'object' ? JSON.stringify(a) : String(a)
    ).join(' ')}`;

    console.error(...args);

    if (this.enabled) {
      this.buffer.push(message);
    }
  }

  private async flush() {
    if (this.buffer.length === 0) return;

    const messages = [...this.buffer];
    this.buffer = [];

    for (const message of messages) {
      try {
        await invoke("write_frontend_log", { message });
      } catch (e) {
        // Don't log errors to avoid infinite loop
      }
    }
  }

  async destroy() {
    if (this.flushInterval) {
      clearInterval(this.flushInterval);
    }
    await this.flush();
  }
}

export const logger = new Logger();

// Initialize logger when imported
logger.init();
