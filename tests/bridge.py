"""Joins a client on one machine to a server port that only listens on another machine's loopback, when only
the server's machine can open connections between the two. Nothing of the server is opened to the network.

  on the client's machine:  bridge.py listen MEET_PORT CLIENT_PORT   (clients connect to 127.0.0.1:CLIENT_PORT)
  on the server's machine:  bridge.py dial MEET_HOST MEET_PORT SERVER_PORT

The dialling side keeps one connection open over which it is told to open another for each client."""
import asyncio, sys


async def pipe(reader, writer):
    try:
        while data := await reader.read(65536):
            writer.write(data)
            await writer.drain()
    except (ConnectionError, asyncio.CancelledError):
        pass
    finally:
        writer.close()


async def join(a, b):
    await asyncio.gather(pipe(a[0], b[1]), pipe(b[0], a[1]))


async def listen(meet_port, client_port):
    control, arrived = None, asyncio.Queue()

    async def met(reader, writer):
        nonlocal control
        if control is None:
            control = writer
        else:
            await arrived.put((reader, writer))

    async def client(reader, writer):
        control.write(b"1")
        await join((reader, writer), await arrived.get())

    await asyncio.start_server(met, "0.0.0.0", meet_port)
    await asyncio.start_server(client, "127.0.0.1", client_port)
    await asyncio.Event().wait()


async def dial(meet_host, meet_port, server_port):
    control, _ = await asyncio.open_connection(meet_host, meet_port)
    tasks = set()
    while await control.read(1):
        there = await asyncio.open_connection(meet_host, meet_port)
        here = await asyncio.open_connection("127.0.0.1", server_port)
        task = asyncio.ensure_future(join(there, here))
        tasks.add(task)
        task.add_done_callback(tasks.discard)


if sys.argv[1] == "listen":
    asyncio.run(listen(int(sys.argv[2]), int(sys.argv[3])))
else:
    asyncio.run(dial(sys.argv[2], int(sys.argv[3]), int(sys.argv[4])))
