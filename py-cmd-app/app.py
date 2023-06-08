import sys
import time

if __name__ == '__main__':
    # loop infinitely and wait for keyboard interrupt
    try:
        while True:
            time.sleep(1)
            print('Hello World!')
    except KeyboardInterrupt:
        print('Exiting...')
        sys.exit(0)