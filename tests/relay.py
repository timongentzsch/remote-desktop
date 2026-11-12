"""A TCP relay, as a proxy or tunnel is one: it takes whatever the host sends at once and passes it on as fast
as its own way out allows. Arguments: the host to relay port 8765 to."""
import asyncio, sys


async def pipe(reader, writer):
    try:
        while data := await reader.read(65536):
            writer.write(data)
            await writer.drain()
    finally:
        writer.close()


async def serve(reader, writer):
    there_reader, there_writer = await asyncio.open_connection(sys.argv[1], 8765)
    await asyncio.gather(pipe(reader, there_writer), pipe(there_reader, writer), return_exceptions=True)


async def main():
    async with await asyncio.start_server(serve, "0.0.0.0", 8765, limit=1 << 20):
        await asyncio.Event().wait()

asyncio.run(main())
