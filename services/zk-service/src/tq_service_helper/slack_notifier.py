import slack_sdk


class SlackChannelNotifier():
    def __init__(self, token:str, channel:str):
        self.client = slack_sdk.WebClient(token)
        self.client.api_test()
        self.channel = channel
        
    
    def notify(self, message:str, **args):
        return self.client.chat_postMessage(channel=self.channel, text=message, **args)
    