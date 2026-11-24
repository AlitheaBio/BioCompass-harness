// TCP proxy adding a fixed one-way delay to every chunk in both directions (RTT = 2 x delay).
// Usage: node latency_proxy.mjs <listenPort> <targetHost> <targetPort> <oneWayDelayMs>
import net from "node:net";

const [listenPort, targetHost, targetPort, delayMs] = process.argv.slice(2);
const delay = Number(delayMs);

function pipeDelayed(from, to) {
  // Preserve ordering: each chunk is released `delay` ms after it arrived.
  from.on("data", (chunk) => {
    setTimeout(() => {
      if (!to.destroyed) to.write(chunk);
    }, delay);
  });
  from.on("end", () => setTimeout(() => to.end(), delay));
  from.on("error", () => to.destroy());
}

net
  .createServer((client) => {
    const upstream = net.connect(Number(targetPort), targetHost);
    pipeDelayed(client, upstream);
    pipeDelayed(upstream, client);
    upstream.on("error", () => client.destroy());
    client.on("error", () => upstream.destroy());
  })
  .listen(Number(listenPort), "127.0.0.1", () => {
    console.log(`latency proxy 127.0.0.1:${listenPort} -> ${targetHost}:${targetPort} (+${delay}ms each way)`);
  });
