import { Database } from '@tursodatabase/database';
import { existsSync, mkdirSync } from 'fs';
import { KvStore } from './kvstore';
import { Filesystem } from './filesystem';
import { ToolCalls } from './toolcalls';

/**
 * Configuration options for opening an AgentFS instance
 */
export interface AgentFSOptions {
  /**
   * Unique identifier for the agent.
   * - If provided without `path`: Creates storage at `.agentfs/{id}.db`
   * - If provided with `path`: Uses the specified path
   */
  id?: string;
  /**
   * Explicit path to the database file.
   * - If provided: Uses the specified path directly
   * - Can be combined with `id`
   */
  path?: string;
}

export class AgentFS {
  private db: Database;

  public readonly kv: KvStore;
  public readonly fs: Filesystem;
  public readonly tools: ToolCalls;

  /**
   * Private constructor - use AgentFS.open() instead
   */
  private constructor(db: Database, kv: KvStore, fs: Filesystem, tools: ToolCalls) {
    this.db = db;
    this.kv = kv;
    this.fs = fs;
    this.tools = tools;
  }

  /**
   * Open an agent filesystem
   * @param options Configuration options (id and/or path required)
   * @returns Fully initialized AgentFS instance
   * @example
   * ```typescript
   * // Using id (creates .agentfs/my-agent.db)
   * const agent = await AgentFS.open({ id: 'my-agent' });
   *
   * // Using id with custom path
   * const agent = await AgentFS.open({ id: 'my-agent', path: './data/mydb.db' });
   *
   * // Using path only
   * const agent = await AgentFS.open({ path: './data/mydb.db' });
   * ```
   */
  static async open(options: AgentFSOptions): Promise<AgentFS> {
    const { id, path } = options;

    // Require at least id or path
    if (!id && !path) {
      throw new Error("AgentFS.open() requires at least 'id' or 'path'.");
    }

    // Validate agent ID if provided
    if (id && !/^[a-zA-Z0-9_-]+$/.test(id)) {
      throw new Error(
        'Agent ID must contain only alphanumeric characters, hyphens, and underscores'
      );
    }

    // Determine database path: explicit path takes precedence, otherwise use id-based path
    let dbPath: string;
    if (path) {
      dbPath = path;
    } else {
      // id is guaranteed to be defined here (we checked !id && !path above)
      const dir = '.agentfs';
      if (!existsSync(dir)) {
        mkdirSync(dir, { recursive: true });
      }
      dbPath = `${dir}/${id}.db`;
    }

    const db = new Database(dbPath);

    // Connect to the database to ensure it's created
    await db.connect();

    return await AgentFS.openWith(db);
  }

  static async openWith(db: Database): Promise<AgentFS> {
    // Create subsystems
    const kv = new KvStore(db);
    const fs = new Filesystem(db);
    const tools = new ToolCalls(db);

    // Wait for all subsystems to initialize
    await kv.ready();
    await fs.ready();
    await tools.ready();

    // Return fully initialized instance
    return new AgentFS(db, kv, fs, tools);
  }

  /**
   * Get the underlying Database instance
   */
  getDatabase(): Database {
    return this.db;
  }

  /**
   * Close the database connection
   */
  async close(): Promise<void> {
    await this.db.close();
  }
}

export { KvStore } from './kvstore';
export { Filesystem } from './filesystem';
export type { Stats } from './filesystem';
export { ToolCalls } from './toolcalls';
export type { ToolCall, ToolCallStats } from './toolcalls';
