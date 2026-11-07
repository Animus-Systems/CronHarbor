export interface StreamMessage {
  id: string;
  type: string;
  data: string;
}
export class SseParser {
  private buffer = "";
  private data: string[] = [];
  private size = 0;
  private id = "";
  private type = "";
  constructor(private readonly limit = 262144) {}
  push(chunk: string): StreamMessage[] {
    this.buffer += chunk;
    const messages: StreamMessage[] = [];
    let end: number;
    while ((end = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, end).replace(/\r$/, "");
      this.buffer = this.buffer.slice(end + 1);
      this.size += line.length;
      if (this.size > this.limit) throw new Error("SSE stream limit exceeded");
      if (!line) {
        if (this.data.length)
          messages.push({
            id: this.id,
            type: this.type || "message",
            data: this.data.join("\n"),
          });
        this.data = [];
        this.type = "";
        this.size = 0;
      } else if (!line.startsWith(":")) {
        const colon = line.indexOf(":");
        const field = colon < 0 ? line : line.slice(0, colon);
        const value = colon < 0 ? "" : line.slice(colon + 1).replace(/^ /, "");
        if (field === "data") this.data.push(value);
        if (field === "event") this.type = value;
        if (field === "id" && !value.includes("\0")) this.id = value;
      }
    }
    if (this.buffer.length + this.size > this.limit)
      throw new Error("SSE stream limit exceeded");
    return messages;
  }
}
